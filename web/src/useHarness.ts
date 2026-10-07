import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, bootstrap, errorText, isAbort, rpc } from "./api";
import { deliverNotice, noticeForEvent, type HarnessNotice } from "./notifications";
import {
  acceptTurn,
  beginTurn,
  consumedCursor,
  emptyTurn,
  isRunningState,
  reduceEvent,
} from "./events";
import type {
  Approval,
  Bootstrap,
  EventBatch,
  Envelope,
  LiveTurn,
  SessionResult,
  SessionSummary,
} from "./types";

export function useHarness(initialData?: Bootstrap) {
  const [data, setData] = useState<Bootstrap>();
  const [connectionError, setConnectionError] = useState("");
  const [eventError, setEventError] = useState("");
  const [error, setError] = useState("");
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [currentId, setCurrentId] = useState<string>();
  const [snapshots, setSnapshots] = useState<Record<string, SessionResult>>({});
  const [live, setLive] = useState<Record<string, LiveTurn>>({});
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [loadingSession, setLoadingSession] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [retry, setRetry] = useState(0);
  const [notices, setNotices] = useState<HarnessNotice[]>([]);
  const mounted = useRef(false);
  const requestSequence = useRef(0);
  const snapshotVersions = useRef(new Map<string, number>());
  const completedTurns = useRef(new Map<string, { sequence: number; requestId?: number }>());
  const liveRef = useRef(live);
  liveRef.current = live;
  const mutateLive = useCallback(
    (
      updater: (previous: Record<string, LiveTurn>) => Record<string, LiveTurn>,
    ) => {
      liveRef.current = updater(liveRef.current);
      setLive(liveRef.current);
    },
    [],
  );
  const beginSnapshot = useCallback((id: string) => {
    const version = (snapshotVersions.current.get(id) || 0) + 1;
    snapshotVersions.current.set(id, version);
    return version;
  }, []);
  const applySnapshot = useCallback((id: string, result: SessionResult, version: number) => {
    if (snapshotVersions.current.get(id) !== version) return false;
    setSnapshots((previous) => ({ ...previous, [id]: result }));
    mutateLive((previous) => {
      const current = previous[id];
      const completed = completedTurns.current.get(id);
      // A newer selection fetch may supersede completion reconciliation. It
      // must also retire that completed stream, but never a new local turn.
      if (current && !current.running && completed &&
          current.lastSequence === completed.sequence && current.requestId === completed.requestId)
        return { ...previous, [id]: { ...current, entries: [], prompt: undefined } };
      if (!current && isRunningState(result.state))
        return { ...previous, [id]: { ...emptyTurn(), running: true, taskId: result.task_id, status: "运行时正在执行" } };
      return previous;
    });
    return true;
  }, [mutateLive]);

  useEffect(() => {
    mounted.current = true;
    for (const id of snapshotVersions.current.keys()) beginSnapshot(id);
    completedTurns.current.clear();
    const abort = new AbortController();
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function initialize() {
      setConnectionError("");
      try {
        const value =
          retry === 0 && initialData
            ? initialData
            : await bootstrap(abort.signal);
        if (!alive) return;
        setData(value);
        setSessions(value.sessions);
        mutateLive((previous) => {
          const next = retry > 0 ? {} : { ...previous };
          for (const session of value.sessions)
            if (!next[session.id] && isRunningState(session.state))
              next[session.id] = {
                ...emptyTurn(),
                running: true,
                taskId: session.task_id,
                status: "运行时正在执行",
              };
          return next;
        });
        setCurrentId((id) => id || value.sessions[0]?.id);
        let cursor = value.latest_sequence;
        async function refreshApprovals() {
          const result = await rpc<{ approvals: Approval[] }>(
            value.token,
            "approvals/list",
            {},
            abort.signal,
          );
          if (alive) setApprovals(result.approvals);
        }
        await refreshApprovals();
        let approvalsDirty = false;
        const pendingFinished = new Map<string, Envelope>();
        const showNotice = (notice: HarnessNotice | undefined) => {
          if (!notice) return;
          deliverNotice(notice);
          setNotices((previous) => [...previous.filter((item) => item.id !== notice.id), notice].slice(-3));
        };
        async function reconcile() {
          if (approvalsDirty) { await refreshApprovals(); approvalsDirty = false; }
          if (!pendingFinished.size || !alive) return;
          const list = await rpc<{ sessions: SessionSummary[] }>(value.token, "sessions/list", {}, abort.signal);
          if (!alive) return;
          setSessions(list.sessions);
          for (const [id, event] of pendingFinished) {
            if (!list.sessions.some((session) => session.id === id)) {
              beginSnapshot(id);
              completedTurns.current.delete(id);
              pendingFinished.delete(id);
              continue;
            }
            const version = beginSnapshot(id);
            const snapshot = await rpc<SessionResult>(value.token, "sessions/get", { id }, abort.signal);
            if (!alive) return;
            if (!applySnapshot(id, snapshot, version)) continue;
            if (snapshot.task_id === event.task_id || !event.task_id)
              showNotice(noticeForEvent(event, snapshot.state));
            pendingFinished.delete(id);
          }
        }
        while (alive) {
          try {
            // Retry durable state reconciliation even if no new event arrives.
            // The event cursor remains monotonic, so notifications are not replayed.
            await reconcile();
            if (!alive) break;
            setEventError("");
            const batch = await rpc<EventBatch>(
              value.token,
              "events/wait",
              { sequence: cursor, limit: 500, timeout_ms: 25000 },
              abort.signal,
            );
            if (!alive) break;
            const previousCursor = cursor;
            cursor = consumedCursor(cursor, batch.events);
            for (const event of batch.events) {
              if (event.sequence <= previousCursor) continue;
              showNotice(noticeForEvent(event));
              const id = event.payload.session_id;
              if (typeof id !== "string") continue;
              if (["agent.done", "agent.server_error"].includes(event.event_type)) {
                pendingFinished.set(id, event);
                approvalsDirty = true;
              }
              if (event.event_type === "agent.approval_needed") approvalsDirty = true;
            }
            setEventError("");
            mutateLive((previous) => {
              const next = { ...previous };
              for (const event of batch.events) {
                const id = event.payload.session_id;
                if (
                  typeof id !== "string" ||
                  !event.event_type.startsWith("agent.")
                )
                  continue;
                next[id] = reduceEvent(next[id], event);
                if (["agent.done", "agent.server_error"].includes(event.event_type)) {
                  beginSnapshot(id); // Invalidate reads started before completion.
                  completedTurns.current.set(id, { sequence: event.sequence, requestId: next[id].requestId });
                }
              }
              return next;
            });
            await reconcile();
          } catch (failure) {
            if (!alive || isAbort(failure)) break;
            if (failure instanceof ApiError && failure.code === 403) {
              setEventError("");
              setConnectionError(
                "本机服务已重启或连接令牌已失效。重新连接后可继续查看会话。",
              );
              break;
            }
            setEventError(errorText(failure));
            await new Promise<void>((resolve) => {
              timer = setTimeout(resolve, 2000);
              abort.signal.addEventListener(
                "abort",
                () => {
                  clearTimeout(timer);
                  resolve();
                },
                { once: true },
              );
            });
          }
        }
      } catch (failure) {
        if (alive && !isAbort(failure)) setConnectionError(errorText(failure));
      }
    }
    void initialize();
    return () => {
      alive = false;
      mounted.current = false;
      abort.abort();
      clearTimeout(timer);
    };
  }, [retry, mutateLive, beginSnapshot, applySnapshot, initialData]);

  useEffect(() => {
    if (!data || !currentId) return;
    const abort = new AbortController();
    const version = beginSnapshot(currentId);
    setLoadingSession(true);
    setError("");
    rpc<SessionResult>(
      data.token,
      "sessions/get",
      { id: currentId },
      abort.signal,
    )
      .then((result) => {
        if (!abort.signal.aborted) {
          applySnapshot(currentId, result, version);
        }
      })
      .catch((failure) => {
        if (!isAbort(failure) && snapshotVersions.current.get(currentId) === version) setError(errorText(failure));
      })
      .finally(() => {
        if (!abort.signal.aborted) setLoadingSession(false);
      });
    return () => abort.abort();
  }, [currentId, data?.token, beginSnapshot, applySnapshot]);

  async function createSession() {
    if (!data || submitting) return;
    setError("");
    setSubmitting(true);
    try {
      const result = await rpc<SessionResult>(data.token, "sessions/create");
      if (!mounted.current) return;
      setSnapshots((previous) => ({
        ...previous,
        [result.session.id]: result,
      }));
      setSessions((previous) => [
        {
          id: result.session.id,
          name: result.session.name,
          archived: false,
          updated_at: null,
          event_count: 0,
          task_id: result.task_id,
          state: result.state,
        },
        ...previous,
      ]);
      setCurrentId(result.session.id);
    } catch (failure) {
      if (mounted.current) setError(errorText(failure));
    } finally {
      if (mounted.current) setSubmitting(false);
    }
  }
  async function send(prompt: string) {
    if (
      !data ||
      submitting ||
      !prompt.trim() ||
      liveRef.current[currentId || ""]?.running ||
      (!liveRef.current[currentId || ""] &&
        isRunningState(snapshots[currentId || ""]?.state))
    )
      return false;
    const serial = ++requestSequence.current;
    setError("");
    setSubmitting(true);
    let id = currentId;
    try {
      if (!id) {
        const result = await rpc<SessionResult>(data.token, "sessions/create");
        id = result.session.id;
        if (!mounted.current) return false;
        setSnapshots((previous) => ({ ...previous, [id!]: result }));
        setCurrentId(id);
      }
      mutateLive((previous) => ({
        ...previous,
        [id!]: beginTurn(previous[id!], serial, prompt),
      }));
      const result = await rpc<{ accepted: boolean; task_id: string | null }>(
        data.token,
        "agent/turn",
        { session_id: id, prompt, wait: false },
      );
      if (!mounted.current || serial !== requestSequence.current) return false;
      mutateLive((previous) => ({
        ...previous,
        [id!]: acceptTurn(previous[id!], serial, result.task_id),
      }));
      // The turn is already accepted. A sidebar refresh failure must not roll
      // back that submission or encourage the user to send it a second time.
      try {
        const list = await rpc<{ sessions: SessionSummary[] }>(
          data.token,
          "sessions/list",
        );
        if (mounted.current) setSessions(list.sessions);
      } catch (failure) {
        if (mounted.current)
          setEventError(`任务已提交，会话列表稍后刷新：${errorText(failure)}`);
      }
      return true;
    } catch (failure) {
      if (mounted.current) {
        setError(errorText(failure));
        if (id)
          mutateLive((previous) =>
            previous[id!]?.requestId === serial
              ? {
                  ...previous,
                  [id!]: {
                    ...previous[id!],
                    running: false,
                    prompt: undefined,
                    error: errorText(failure),
                  },
                }
              : previous,
          );
      }
      return false;
    } finally {
      if (mounted.current) setSubmitting(false);
    }
  }
  async function stop() {
    if (!data || !currentId) return;
    const taskId =
      liveRef.current[currentId]?.taskId || snapshots[currentId]?.task_id;
    if (!taskId) {
      setError("任务尚未登记，请稍后再暂停。");
      return;
    }
    try {
      await rpc(data.token, "tasks/pause", { id: taskId });
      mutateLive((previous) => ({
        ...previous,
        [currentId]: {
          ...(previous[currentId] || emptyTurn()),
          status: "已请求暂停，等待运行时确认",
        },
      }));
    } catch (failure) {
      setError(errorText(failure));
    }
  }
  async function resolveApproval(requestId: string, allow: boolean) {
    if (!data) return;
    try {
      await rpc(data.token, "approvals/resolve", {
        request_id: requestId,
        allow,
      });
      setApprovals((previous) =>
        previous.filter((item) => item.request_id !== requestId),
      );
    } catch (failure) {
      setError(errorText(failure));
    }
  }
  async function refreshSessions() {
    if (!data) return;
    const result = await rpc<{ sessions: SessionSummary[] }>(data.token, "sessions/list");
    if (!mounted.current) return;
    setSessions(result.sessions);
    setCurrentId((id) => result.sessions.some((item) => item.id === id) ? id : result.sessions[0]?.id);
    const ids = new Set(result.sessions.map((item) => item.id));
    for (const id of snapshotVersions.current.keys()) {
      if (!ids.has(id)) {
        beginSnapshot(id);
        completedTurns.current.delete(id);
      }
    }
    setSnapshots((previous) => Object.fromEntries(Object.entries(previous).filter(([id]) => ids.has(id))));
    mutateLive((previous) => Object.fromEntries(Object.entries(previous).filter(([id]) => ids.has(id))));
  }
  return {
    data,
    setData,
    connectionError,
    eventError,
    error,
    setError,
    sessions,
    currentId,
    selectSession: setCurrentId,
    snapshot: currentId ? snapshots[currentId] : undefined,
    live: currentId ? live[currentId] : undefined,
    approvals,
    loadingSession,
    submitting,
    createSession,
    send,
    stop,
    resolveApproval,
    refreshSessions,
    notices,
    dismissNotice: (id: number) => setNotices((previous) => previous.filter((item) => item.id !== id)),
    reconnect: () => setRetry((value) => value + 1),
  };
}
