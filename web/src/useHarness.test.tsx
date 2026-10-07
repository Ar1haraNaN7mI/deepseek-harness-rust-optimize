import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useHarness } from "./useHarness";
import * as api from "./api";
import type { Bootstrap, EventBatch, SessionResult } from "./types";

vi.mock("./api", async (original) => ({
  ...(await original<typeof import("./api")>()),
  rpc: vi.fn(),
  bootstrap: vi.fn(),
}));
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});
const data: Bootstrap = {
  token: "test",
  workspace: "test",
  profile: { username: "Operator", badge_id: "01" },
  model: { name: "test", backend: "test", ready: true, local: true },
  status: "ready",
  startup: {
    enabled: false,
    next_enabled: null,
    sound: false,
    interactive: true,
    reduced_motion: false,
  },
  inventory_mode: "mounted",
  skills: [],
  plugins: [],
  latest_sequence: 10,
  sessions: [
    {
      id: "s1",
      name: null,
      archived: false,
      updated_at: null,
      event_count: 0,
      task_id: null,
      state: null,
    },
  ],
};
const snapshot = {
  session: { id: "s1", name: null, events: [] },
  task_id: null,
  state: null,
};
describe("Harness request lifecycle", () => {
  it("does not let an older selection snapshot replace completion or erase a newer turn", async () => {
    let finishInitial!: (value: SessionResult) => void;
    let deliverEvents!: (value: EventBatch) => void;
    let reads = 0;
    const completed: SessionResult = {
      ...snapshot, task_id: "t1", state: "completed",
      session: { ...snapshot.session, events: [{ id: "answer", type: "assistant_message", text: "Final reply", at: "2026-10-07T00:00:00Z" }] },
    };
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "sessions/get") {
        if (++reads === 1) return new Promise((resolve) => { finishInitial = resolve; });
        return completed;
      }
      if (method === "approvals/list") return { approvals: [] };
      if (method === "events/wait") return new Promise((resolve) => { deliverEvents = resolve; });
      if (method === "sessions/list") return { sessions: data.sessions };
      if (method === "agent/turn") return { accepted: true, task_id: "t2" };
      throw new Error(method);
    });
    const { result } = renderHook(() => useHarness(data));
    await waitFor(() => expect(finishInitial).toBeTypeOf("function"));
    await act(async () => deliverEvents({ latest_sequence: 12, events: [
      { sequence: 11, event_type: "agent.text_delta", occurred_at: "", task_id: "t1", payload: { session_id: "s1", text: "Final reply" } },
      { sequence: 12, event_type: "agent.done", occurred_at: "", task_id: "t1", payload: { session_id: "s1" } },
    ] }));
    await waitFor(() => expect(result.current.snapshot?.session.events).toHaveLength(1));
    await act(async () => { expect(await result.current.send("New task")).toBe(true); });
    await act(async () => finishInitial(snapshot));
    expect(result.current.snapshot?.session.events).toHaveLength(1);
    expect(result.current.live?.running).toBe(true);
    expect(result.current.live?.prompt).toBe("New task");
  });

  it("lets a newer selection retire a finished stream and invalidates reads for deleted sessions", async () => {
    let finishReconcile!: (value: SessionResult) => void;
    let deliverEvents!: (value: EventBatch) => void;
    let reads = 0;
    let deleted = false;
    const other = { ...data.sessions[0], id: "s2" };
    const initial = { ...data, sessions: [...data.sessions, other] };
    const completed: SessionResult = {
      ...snapshot, task_id: "t1", state: "completed",
      session: { ...snapshot.session, events: [{ id: "answer", type: "assistant_message", text: "Final reply", at: "2026-10-07T00:00:00Z" }] },
    };
    vi.mocked(api.rpc).mockImplementation(async (_token, method, params) => {
      if (method === "sessions/get") {
        if (params?.id === "s2") return { ...snapshot, session: { ...snapshot.session, id: "s2" } };
        if (deleted) throw new Error("Session deleted");
        if (++reads === 2) return new Promise((resolve) => { finishReconcile = resolve; });
        return reads > 2 ? completed : snapshot;
      }
      if (method === "approvals/list") return { approvals: [] };
      if (method === "events/wait") return new Promise((resolve) => { deliverEvents = resolve; });
      if (method === "sessions/list") return { sessions: deleted ? [other] : initial.sessions };
      throw new Error(method);
    });
    const { result } = renderHook(() => useHarness(initial));
    await waitFor(() => expect(result.current.snapshot).toBeTruthy());
    await act(async () => deliverEvents({ latest_sequence: 12, events: [
      { sequence: 11, event_type: "agent.text_delta", occurred_at: "", task_id: "t1", payload: { session_id: "s1", text: "Final reply" } },
      { sequence: 12, event_type: "agent.done", occurred_at: "", task_id: "t1", payload: { session_id: "s1" } },
    ] }));
    await waitFor(() => expect(finishReconcile).toBeTypeOf("function"));
    act(() => result.current.selectSession("s2"));
    await waitFor(() => expect(result.current.snapshot?.session.id).toBe("s2"));
    act(() => result.current.selectSession("s1"));
    await waitFor(() => expect(result.current.snapshot?.session.events).toHaveLength(1));
    expect(result.current.live?.entries).toHaveLength(0);
    deleted = true;
    await act(async () => { await result.current.refreshSessions(); });
    await act(async () => finishReconcile(completed));
    act(() => result.current.selectSession("s1"));
    await waitFor(() => expect(result.current.error).toContain("deleted"));
    expect(result.current.snapshot).toBeUndefined();
    expect(result.current.live).toBeUndefined();
  });

  it("retries an approval refresh after its event was consumed, without duplicate notifications", async () => {
    let approvalReads = 0;
    let eventReads = 0;
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "sessions/get") return snapshot;
      if (method === "approvals/list") {
        approvalReads++;
        if (approvalReads === 2) throw new Error("temporary approval fetch failure");
        return { approvals: approvalReads > 2 ? [{ request_id:"a1",call_id:"call1",name:"shell",summary:"fixture",task_id:"t1",run_id:null }] : [] };
      }
      if (method === "events/wait") {
        if (++eventReads > 1) return new Promise(() => {});
        return { latest_sequence:11, events:[{sequence:11,event_type:"agent.approval_needed",occurred_at:"",task_id:"t1",payload:{session_id:"s1"}}] };
      }
      throw new Error(method);
    });
    const { result } = renderHook(() => useHarness(data));
    await waitFor(() => expect(result.current.approvals).toHaveLength(1), {timeout:3500});
    expect(approvalReads).toBe(3);
    expect(result.current.notices).toHaveLength(1);
  });
  it("does not undo an accepted task when the following sidebar refresh fails", async () => {
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "sessions/get") return snapshot;
      if (method === "approvals/list") return { approvals: [] };
      if (method === "events/wait") return new Promise(() => {});
      if (method === "agent/turn") return { accepted: true, task_id: "t1" };
      if (method === "sessions/list") throw new Error("sidebar unavailable");
      throw new Error(method);
    });
    const { result } = renderHook(() => useHarness(data));
    await waitFor(() => expect(result.current.snapshot).toBeTruthy());
    let submitted = false;
    await act(async () => {
      submitted = await result.current.send("real prompt");
    });
    expect(submitted).toBe(true);
    expect(result.current.live?.running).toBe(true);
    expect(result.current.error).toBe("");
    expect(result.current.eventError).toContain("任务已提交");
  });
  it("keeps a turn completed when its event stream beats the acceptance response", async () => {
    let finishAccepted!: (value: unknown) => void;
    let deliverEvents!: (value: EventBatch) => void;
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "sessions/get") return snapshot;
      if (method === "approvals/list") return { approvals: [] };
      if (method === "events/wait")
        return new Promise((resolve) => {
          deliverEvents = resolve;
        });
      if (method === "agent/turn")
        return new Promise((resolve) => {
          finishAccepted = resolve;
        });
      if (method === "sessions/list") return { sessions: data.sessions };
      throw new Error(method);
    });
    const { result } = renderHook(() => useHarness(data));
    await waitFor(() => expect(result.current.snapshot).toBeTruthy());
    let submission!: Promise<boolean>;
    act(() => {
      submission = result.current.send("fast");
    });
    await waitFor(() => expect(finishAccepted).toBeTypeOf("function"));
    await act(async () =>
      deliverEvents({
        latest_sequence: 11,
        events: [
          {
            sequence: 11,
            event_type: "agent.done",
            occurred_at: "",
            task_id: "t1",
            payload: { session_id: "s1" },
          },
        ],
      }),
    );
    expect(result.current.live?.running).toBe(false);
    await act(async () => {
      finishAccepted({ accepted: true, task_id: "t1" });
      await submission;
    });
    expect(result.current.live?.running).toBe(false);
    expect(result.current.live?.prompt).toBeUndefined();
  });
  it("hydrates a running task and refuses a duplicate send before any new event", async () => {
    const running = {
      ...data,
      sessions: [
        { ...data.sessions[0], task_id: "t1", state: "waiting_approval" },
      ],
    };
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "sessions/get")
        return { ...snapshot, task_id: "t1", state: "waiting_approval" };
      if (method === "approvals/list") return { approvals: [] };
      if (method === "events/wait") return new Promise(() => {});
      throw new Error(method);
    });
    const { result } = renderHook(() => useHarness(running));
    await waitFor(() => expect(result.current.live?.running).toBe(true));
    expect(result.current.live?.taskId).toBe("t1");
    let accepted = true;
    await act(async () => {
      accepted = await result.current.send("duplicate");
    });
    expect(accepted).toBe(false);
    expect(
      vi.mocked(api.rpc).mock.calls.some((call) => call[1] === "agent/turn"),
    ).toBe(false);
  });
  it("stops polling an expired token and reconnects with a fresh bootstrap without resending", async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({ ...data, token: "fresh" });
    vi.mocked(api.rpc).mockImplementation(async (token, method) => {
      if (method === "sessions/get") return snapshot;
      if (method === "approvals/list") return { approvals: [] };
      if (method === "events/wait" && token === "test")
        throw new api.ApiError("expired", 403);
      if (method === "events/wait") return new Promise(() => {});
      throw new Error(method);
    });
    const { result } = renderHook(() => useHarness(data));
    await waitFor(() =>
      expect(result.current.connectionError).toContain("令牌已失效"),
    );
    expect(
      vi
        .mocked(api.rpc)
        .mock.calls.filter(
          (call) => call[1] === "events/wait" && call[0] === "test",
        ),
    ).toHaveLength(1);
    act(() => result.current.reconnect());
    await waitFor(() => expect(result.current.data?.token).toBe("fresh"));
    expect(result.current.connectionError).toBe("");
    expect(
      vi.mocked(api.rpc).mock.calls.some((call) => call[1] === "agent/turn"),
    ).toBe(false);
  });
});
