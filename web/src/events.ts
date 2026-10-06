import type { Envelope, LiveTurn } from "./types";

export const emptyTurn = (): LiveTurn => ({
  running: false,
  taskId: null,
  turnId: null,
  entries: [],
  lastSequence: 0,
});
export const isRunningState = (state: string | null | undefined) =>
  ["running", "queued", "waiting_approval", "waiting_event"].includes(
    state || "",
  );
export function beginTurn(
  previous: LiveTurn | undefined,
  requestId: number,
  prompt: string,
): LiveTurn {
  return {
    ...emptyTurn(),
    lastSequence: previous?.lastSequence || 0,
    requestId,
    prompt,
    running: true,
    status: "正在提交",
  };
}
export function acceptTurn(
  current: LiveTurn,
  requestId: number,
  taskId: string | null,
): LiveTurn {
  // Events may finish the turn before the HTTP acknowledgement arrives.
  // Acknowledging a request must never change its event-derived lifecycle.
  return current.requestId === requestId
    ? { ...current, taskId: current.taskId || taskId }
    : current;
}
export function reduceEvent(
  previous: LiveTurn | undefined,
  event: Envelope,
): LiveTurn {
  const state = previous || emptyTurn();
  if (event.sequence <= state.lastSequence) return state;
  const next = {
    ...state,
    entries: [...state.entries],
    lastSequence: event.sequence,
    taskId: event.task_id || state.taskId,
  };
  const payload = event.payload;
  switch (event.event_type) {
    case "agent.turn_started":
      if (state.turnId && state.turnId !== event.turn_id) next.entries = [];
      return {
        ...next,
        turnId: event.turn_id || String(payload.turn_id || ""),
        running: true,
        error: undefined,
        status: "正在处理",
      };
    case "agent.text_delta": {
      const text = typeof payload.text === "string" ? payload.text : "";
      const last = next.entries.at(-1);
      if (last?.kind === "text")
        next.entries[next.entries.length - 1] = {
          ...last,
          text: last.text + text,
        };
      else
        next.entries.push({ id: `text-${event.sequence}`, kind: "text", text });
      return { ...next, running: true, status: "正在回复" };
    }
    case "agent.reasoning_delta":
      return { ...next, running: true, status: "模型正在处理" };
    case "agent.tool_started":
      next.entries.push({
        id: String(payload.call_id),
        kind: "tool",
        name: String(payload.name),
        text: "",
        finished: false,
      });
      return { ...next, running: true, status: "正在调用工具" };
    case "agent.tool_finished": {
      const id = String(payload.call_id);
      const tool = {
        id,
        kind: "tool" as const,
        name: String(payload.name),
        text: String(payload.preview || ""),
        ok: payload.ok === true,
        finished: true,
      };
      const index = next.entries.findIndex((entry) => entry.id === id);
      if (index >= 0) next.entries[index] = tool;
      else next.entries.push(tool);
      return next;
    }
    case "agent.approval_needed":
      return { ...next, running: true, status: "等待工具授权" };
    case "agent.error":
    case "agent.server_error":
      return {
        ...next,
        error: String(payload.message || payload.error || "执行失败"),
        running: false,
        status: "执行中断",
      };
    case "agent.done":
      return { ...next, running: false, status: undefined };
    default:
      return next;
  }
}
// latest_sequence may point beyond a limited batch. Advancing only through the
// records actually consumed prevents silently dropping the next page.
export function consumedCursor(previous: number, events: Envelope[]) {
  return events.reduce(
    (cursor, event) => Math.max(cursor, event.sequence),
    previous,
  );
}
