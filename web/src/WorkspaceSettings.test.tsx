import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WorkspaceSettings } from "./WorkspaceSettings";
import * as api from "./api";

vi.mock("./api", async (original) => ({ ...(await original<typeof import("./api")>()), rpc: vi.fn() }));

const task = { id: "task-fixture", session_id: "session-fixture", goal: { outcome: "Check fixture behavior" }, state: "running", updated_at: "2026-10-07T00:00:00Z" };
const workspace = { workspace: "/fixture", outer_home: "/fixture/outer", workspace_outer: "/fixture/.dsh", git: { available: true, branch: "test", status: " M file.txt" }, permissions: "default", sandbox: "workspace-write", approval: "on-request" };
const output = { session: { id: "session-fixture", events: [{ id: "answer", type: "assistant_message", text: "Fixture review completed.", at: "2026-10-07T00:00:00Z" }] }, state: "completed", task_id: "task-fixture" };

beforeEach(() => {
  vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
    if (method === "workspace/get") return workspace;
    if (method === "tasks/list") return { tasks: [task] };
    if (method === "sessions/get") return output;
    if (method === "agent/turn") return { accepted: true, session_id: "session-fixture", task_id: "task-fixture" };
    if (method === "cloud/list") return { artifacts: [] };
    return {};
  });
});
afterEach(() => { cleanup(); vi.resetAllMocks(); vi.useRealTimers(); });

describe("DSH workspace settings", () => {
  it("reads real runtime paths and Git status without any execution or cloud request", async () => {
    render(<WorkspaceSettings token="fixture-token" section="workspace" />);
    expect(await screen.findByText("/fixture/outer")).toBeTruthy();
    expect(screen.getByLabelText("Git 工作区状态").textContent).toBe(" M file.txt");
    expect(api.rpc).not.toHaveBeenCalledWith("fixture-token", "agent/turn", expect.anything());
  });

  it("submits a real task and displays the persisted assistant output", async () => {
    const onChanged = vi.fn();
    render(<WorkspaceSettings token="fixture-token" section="tasks" onChanged={onChanged} />);
    fireEvent.change(screen.getByLabelText("任务说明"), { target: { value: "  Verify this fixture  " } });
    fireEvent.click(screen.getByRole("button", { name: "开始任务" }));
    expect(await screen.findByText("Fixture review completed.")).toBeTruthy();
    expect(api.rpc).toHaveBeenCalledWith("fixture-token", "agent/turn", { prompt: "Verify this fixture", wait: false });
    expect(onChanged).toHaveBeenCalled();
  });

  it("uses durable task controls and waits for an explicit cancellation", async () => {
    render(<WorkspaceSettings token="fixture-token" section="tasks" />);
    fireEvent.click(await screen.findByRole("button", { name: "暂停任务" }));
    await waitFor(() => expect(api.rpc).toHaveBeenCalledWith("fixture-token", "tasks/pause", { id: task.id }));
    await waitFor(() => expect((screen.getByRole("button", { name: "取消任务" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "取消任务" }));
    expect(api.rpc).not.toHaveBeenCalledWith("fixture-token", "tasks/cancel", expect.anything());
    fireEvent.click(screen.getByRole("button", { name: "确认取消任务" }));
    await waitFor(() => expect(api.rpc).toHaveBeenCalledWith("fixture-token", "tasks/cancel", { id: task.id }));
  });

  it("previews server-read Git diff before running exactly the prepared review", async () => {
    const prepared = { label: "Commit fixture", status: "", diff: "+ actual changed code", prompt: "Review the captured fixture diff without editing files." };
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "review/prepare") return prepared;
      if (method === "agent/turn") return { accepted: true, session_id: "session-fixture", task_id: "task-fixture" };
      if (method === "sessions/get") return output;
      return {};
    });
    render(<WorkspaceSettings token="fixture-token" section="review" />);
    fireEvent.change(screen.getByLabelText("审查范围"), { target: { value: "commit" } });
    fireEvent.change(screen.getByLabelText("Git 引用"), { target: { value: "HEAD" } });
    fireEvent.click(screen.getByRole("button", { name: "读取审查差异" }));
    expect(await screen.findByLabelText("审查差异")).toHaveProperty("textContent", prepared.diff);
    expect(api.rpc).not.toHaveBeenCalledWith("fixture-token", "agent/turn", expect.anything());
    fireEvent.click(screen.getByRole("button", { name: "开始代码审查" }));
    expect(await screen.findByText("Fixture review completed.")).toBeTruthy();
    expect(api.rpc).toHaveBeenCalledWith("fixture-token", "review/prepare", { scope: "commit", reference: "HEAD", instructions: "" });
    expect(api.rpc).toHaveBeenCalledWith("fixture-token", "agent/turn", { prompt: prepared.prompt, wait: false });
  });

  it("drops stale review previews after the review scope changes", async () => {
    vi.mocked(api.rpc).mockResolvedValue({ label: "Old diff", diff: "old", status: "", prompt: "old prompt" });
    render(<WorkspaceSettings token="fixture-token" section="review" />);
    fireEvent.click(screen.getByRole("button", { name: "读取审查差异" }));
    await screen.findByLabelText("审查差异");
    fireEvent.change(screen.getByLabelText("额外审查要求"), { target: { value: "new criteria" } });
    expect(screen.queryByRole("button", { name: "开始代码审查" })).toBeNull();
  });

  it("reconciles a terminal review snapshot that arrived before the final answer", async () => {
    vi.useFakeTimers();
    let reads = 0;
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "review/prepare") return { label: "Review fixture", status: "", diff: "+ changed", prompt: "review fixture" };
      if (method === "agent/turn") return { accepted: true, session_id: "session-fixture", task_id: "task-fixture" };
      if (method === "sessions/get") return ++reads === 1 ? { ...output, session: { ...output.session, events: [] } } : output;
      return {};
    });
    render(<WorkspaceSettings token="fixture-token" section="review" />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "读取审查差异" })); });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "开始代码审查" })); });
    expect(screen.queryByText("Fixture review completed.")).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(screen.getByText("Fixture review completed.")).toBeTruthy();
    expect(reads).toBe(2);
  });

  it("keeps a failed real review request visible and never claims it started", async () => {
    vi.mocked(api.rpc).mockRejectedValue(new Error("Git reference does not exist"));
    render(<WorkspaceSettings token="fixture-token" section="review" />);
    fireEvent.click(screen.getByRole("button", { name: "读取审查差异" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Git reference does not exist");
    expect(screen.queryByRole("button", { name: "开始代码审查" })).toBeNull();
  });

  it("dry-runs a stored patch and requires a second confirmation before writing files", async () => {
    const patch = { id: "artifact-fixture", source: "fix.patch", patch: "*** Update File: file.txt", workspace: "/fixture", sha256: "digest" };
    vi.mocked(api.rpc).mockImplementation(async (_token, method, params) => {
      if (method === "workspace/get") return workspace;
      if (method === "cloud/list") return { artifacts: [patch] };
      if (method === "workspace/artifact_apply") return { artifact_id: patch.id, dry_run: params?.dry_run, files: ["file.txt"], hunks: 1, operations: ["update"], ...(params?.dry_run === false ? { report: "Applied fixture patch" } : {}) };
      return {};
    });
    render(<WorkspaceSettings token="fixture-token" section="workspace" />);
    fireEvent.click(await screen.findByRole("button", { name: "预览 fix.patch" }));
    fireEvent.click(await screen.findByRole("button", { name: "应用此补丁" }));
    expect(api.rpc).toHaveBeenCalledWith("fixture-token", "workspace/artifact_apply", { id: patch.id, dry_run: true });
    expect(api.rpc).not.toHaveBeenCalledWith("fixture-token", "workspace/artifact_apply", { id: patch.id, dry_run: false });
    fireEvent.click(screen.getByRole("button", { name: "确认写入文件" }));
    expect(await screen.findByText("Applied fixture patch")).toBeTruthy();
    expect(api.rpc).toHaveBeenCalledWith("fixture-token", "workspace/artifact_apply", { id: patch.id, dry_run: false });
  });
});
