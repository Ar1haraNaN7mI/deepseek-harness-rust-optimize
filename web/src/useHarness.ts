import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, bootstrap, errorText, isAbort, rpc } from "./api";
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
  const mounted = useRef(false);
  const requestSequence = useRef(0);
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

  useEffect(() => {
    mounted.current = true;
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
        while (alive) {
          try {
            const batch = await rpc<EventBatch>(
              value.token,
              "events/wait",
              { sequence: cursor, limit: 500, timeout_ms: 25000 },
              abort.signal,
            );
            if (!alive) break;
            cursor = consumedCursor(cursor, batch.events);
            setEventError("");
            const finished = new Map<string, number>();
            let approvalChanged = false;
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
                if (
                  ["agent.done", "agent.server_error"].includes(
                    event.event_type,
                  )
                )
                  finished.set(id, event.sequence);
                if (event.event_type === "agent.approval_needed")
                  approvalChanged = true;
              }
              return next;
            });
            if (approvalChanged || finished.size) await refreshApprovals();
            for (const [id, endSequence] of finished) {
              const snapshot = await rpc<SessionResult>(
                value.token,
                "sessions/get",
                { id },
                abort.signal,
              );
              if (!alive) break;
              // A later turn may already be streaming; never erase its live data.
              setSnapshots((previous) => ({ ...previous, [id]: snapshot }));
              mutateLive((previous) =>
                previous[id]?.lastSequence === endSequence
                  ? {
                      ...previous,
                      [id]: { ...previous[id], entries: [], prompt: undefined },
                    }
                  : previous,
              );
            }
            if (finished.size) {
              const list = await rpc<{ sessions: SessionSummary[] }>(
                value.token,
                "sessions/list",
                {},
                abort.signal,
              );
              if (alive) setSessions(list.sessions);
            }
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
  }, [retry, mutateLive, initialData]);

  useEffect(() => {
    if (!data || !currentId) return;
    const abort = new AbortController();
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
          setSnapshots((previous) => ({ ...previous, [currentId]: result }));
          mutateLive((previous) =>
            !previous[currentId] && isRunningState(result.state)
              ? {
                  ...previous,
                  [currentId]: {
                    ...emptyTurn(),
                    running: true,
                    taskId: result.task_id,
                    status: "运行时正在执行",
                  },
                }
              : previous,
          );
        }
      })
      .catch((failure) => {
        if (!isAbort(failure)) setError(errorText(failure));
      })
      .finally(() => {
        if (!abort.signal.aborted) setLoadingSession(false);
      });
    return () => abort.abort();
  }, [currentId, data?.token, mutateLive]);

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
    reconnect: () => setRetry((value) => value + 1),
  };
}
