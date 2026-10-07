import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useHarness } from "./useHarness";
import * as api from "./api";
import type { Bootstrap, EventBatch } from "./types";

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
