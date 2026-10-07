import { describe, expect, it } from "vitest";
import {
  acceptTurn,
  beginTurn,
  consumedCursor,
  isRunningState,
  reduceEvent,
} from "./events";
import type { Envelope } from "./types";

const event = (sequence: number, type: string, payload = {}): Envelope => ({
  sequence,
  event_type: type,
  task_id: "task-1",
  turn_id: "turn-1",
  occurred_at: "",
  payload: { session_id: "session-1", ...payload },
});
describe("runtime stream projection", () => {
  it("preserves reasoning separately so display preferences never mix it into the answer", () => {
    let state = reduceEvent(undefined, event(1, "agent.reasoning_delta", {text:"consider "}));
    state = reduceEvent(state, event(2, "agent.reasoning_delta", {text:"options"}));
    state = reduceEvent(state, event(3, "agent.text_delta", {text:"answer"}));
    expect(state.entries.map(({kind,text}) => ({kind,text}))).toEqual([
      {kind:"reasoning",text:"consider options"}, {kind:"text",text:"answer"},
    ]);
  });
  it("keeps text and tool output in their real interleaved order and deduplicates records", () => {
    let state = reduceEvent(undefined, event(1, "agent.turn_started"));
    state = reduceEvent(state, event(2, "agent.text_delta", { text: "开始" }));
    state = reduceEvent(
      state,
      event(3, "agent.tool_started", { name: "read_file", call_id: "call-1" }),
    );
    state = reduceEvent(
      state,
      event(4, "agent.tool_finished", {
        name: "read_file",
        call_id: "call-1",
        ok: true,
        preview: "真实文件内容",
      }),
    );
    state = reduceEvent(state, event(5, "agent.text_delta", { text: "完成" }));
    expect(state.entries.map((item) => item.kind)).toEqual([
      "text",
      "tool",
      "text",
    ]);
    expect(state.entries[1].text).toBe("真实文件内容");
    expect(
      reduceEvent(state, event(5, "agent.text_delta", { text: "完成" })),
    ).toBe(state);
    expect(reduceEvent(state, event(6, "agent.done")).running).toBe(false);
  });
  it("does not revive a fast completed turn when its accepted HTTP response arrives late", () => {
    let state = beginTurn(undefined, 1, "hi");
    state = reduceEvent(state, event(1, "agent.turn_started"));
    state = reduceEvent(state, event(2, "agent.text_delta", { text: "hello" }));
    state = reduceEvent(state, event(3, "agent.done"));
    const accepted = acceptTurn(state, 1, "task-1");
    expect(accepted.running).toBe(false);
    expect(accepted.entries[0].text).toBe("hello");
    expect(accepted.lastSequence).toBe(3);
    const newRequest = beginTurn(accepted, 2, "next");
    expect(acceptTurn(newRequest, 1, "old-task")).toBe(newRequest);
  });
  it("preserves a runtime error even if the accepted response arrives afterward", () => {
    const pending = beginTurn(undefined, 1, "hello");
    const failed = reduceEvent(
      pending,
      event(3, "agent.server_error", { error: "missing credentials" }),
    );
    expect(acceptTurn(failed, 1, null)).toMatchObject({
      running: false,
      error: "missing credentials",
    });
  });
  it("consumes only the actual batch and recognizes recovered running states", () => {
    expect(consumedCursor(8, [event(9, "a"), event(10, "b")])).toBe(10);
    expect(consumedCursor(8, [])).toBe(8);
    expect(isRunningState("waiting_approval")).toBe(true);
    expect(isRunningState("paused")).toBe(false);
  });
});
