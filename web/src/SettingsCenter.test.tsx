import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsCenter } from "./SettingsCenter";
import { resetUiPreferences } from "./uiPreferences";
import type { useHarness } from "./useHarness";
import type { Bootstrap } from "./types";
import * as api from "./api";

vi.mock("./api", async (original) => ({
  ...(await original<typeof import("./api")>()),
  rpc: vi.fn(),
  post: vi.fn(),
  bootstrap: vi.fn(),
}));
vi.mock("./Emblem", () => ({ Emblem: () => <span>DSH</span> }));
vi.mock("./ModelServiceSettings", () => ({
  ModelServiceSettings: ({ token, onChanged }: { token: string; onChanged: () => void }) => (
    <button onClick={onChanged} data-token={token}>保存 DSH 模型连接</button>
  ),
}));
vi.mock("./ExtensionSettings", () => ({
  ExtensionSettings: ({ token }: { token: string }) => <div data-token={token}>DSH 扩展管理</div>,
}));
vi.mock("./WorkspaceSettings", () => ({
  WorkspaceSettings: ({ token, section }: { token: string; section: string }) => (
    <div data-token={token}>DSH 本机 {section}</div>
  ),
}));

const session = {
  id: "s1",
  name: "真实聊天",
  archived: false,
  updated_at: null,
  event_count: 4,
  task_id: null,
  state: null,
};
const data: Bootstrap = {
  token: "local-token",
  workspace: "/fixture",
  profile: { username: "Operator", badge_id: "01" },
  model: { name: "model-a", backend: "local", ready: true, local: true },
  status: "ready",
  startup: {
    enabled: false,
    next_enabled: null,
    sound: true,
    interactive: true,
    reduced_motion: false,
  },
  inventory_mode: "mounted",
  skills: [],
  plugins: [],
  sessions: [session],
  latest_sequence: 5,
};
const snapshot = {
  settings: {
    model: "model-a",
    thinking: false,
    personality: "default",
    custom_instructions: "",
    memory_inject: true,
    memory_generate: true,
    approval: "on-request",
    sandbox: "workspace-write",
    security_research_mode: false,
  },
  effective: {
    model: "model-a",
    thinking: false,
    backend: "local",
    permissions: "default",
    sandbox: "workspace-write",
    approval: "on-request",
    memory_available: false,
    memory_inject: false,
    memory_generate: false,
  },
  account: { kind: "local", credential_configured: true, model_ready: true },
  storage: {
    session_count: 1,
    archived_count: 0,
    session_bytes: 600,
    memory_bytes: 100,
    event_bytes: 500,
    total_bytes: 1200,
  },
  usage: {
    session_count: 1,
    message_count: 12,
    user_message_count: 6,
    assistant_message_count: 6,
    tool_call_count: 4,
    event_count: 24,
    token_usage_available: false,
  },
  capabilities: {
    plugin_toggle: false,
    mcp_connect: false,
    cloud_account: false,
  },
  plugins: [],
  mcp: { servers: [], connection_supported: false },
};
const emptyMemory = { memories: [], feedback: [], total: 0, feedback_total: 0 };
function baseRpc(method: string) {
  if (method === "settings/get" || method === "settings/update")
    return snapshot;
  if (method === "sessions/list") return { sessions: [session] };
  if (method === "memory/list") return emptyMemory;
  return {};
}
function harness() {
  return {
    data,
    setData: vi.fn(),
    refreshSessions: vi.fn().mockResolvedValue(undefined),
  } as unknown as ReturnType<typeof useHarness>;
}
async function ready() {
  await waitFor(() =>
    expect(
      (screen.getByLabelText("默认模型") as HTMLInputElement).disabled,
    ).toBe(false),
  );
}
function category(name: string) {
  fireEvent.click(
    within(screen.getByRole("navigation", { name: "设置分类" })).getByRole(
      "button",
      { name },
    ),
  );
}
beforeEach(() => {
  localStorage.clear();
  resetUiPreferences();
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
  vi.mocked(api.rpc).mockImplementation(async (_token, method) =>
    baseRpc(method),
  );
  vi.mocked(api.bootstrap).mockResolvedValue(data);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.resetAllMocks();
});

describe("Settings center", () => {
  it("closes and cancels a nested deletion confirmation when the outer settings closes", async () => {
    const props = {
      harness: harness(),
      onReplay: vi.fn(),
      onOpenChange: vi.fn(),
    };
    const view = render(<SettingsCenter {...props} open />);
    await ready();
    category("数据控制");
    fireEvent.click(screen.getByRole("button", { name: "全部删除" }));
    expect(screen.getByRole("button", { name: "确认" })).toBeTruthy();
    view.rerender(<SettingsCenter {...props} open={false} />);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.rpc).not.toHaveBeenCalledWith(
      "local-token",
      "sessions/delete_all",
      {},
    );
    view.rerender(<SettingsCenter {...props} open />);
    expect(screen.queryByRole("button", { name: "确认" })).toBeNull();
  });
  it("shows only DSH settings and routes native tools without third-party account placeholders", async () => {
    render(
      <SettingsCenter
        harness={harness()}
        onReplay={vi.fn()}
        open
        onOpenChange={vi.fn()}
      />,
    );
    await ready();
    const navigation = screen.getByRole("navigation", { name: "设置分类" });
    const names = within(navigation).getAllByRole("button").map((button) => button.textContent);
    expect(names).toEqual([
      "通用", "外观", "通知", "个人资料", "执行权限", "模型服务", "已归档聊天",
      "语音", "存储", "个性化", "宠物", "键盘", "用量", "数据控制", "插件与 Skills",
      "本机工作区", "DSH 任务", "代码审查", "启动动画",
    ]);
    for (const name of names) {
      category(name!);
      expect(screen.getByRole("dialog").textContent).not.toMatch(/ChatGPT|Codex|OpenAI|尚未接入|云服务功能/);
    }
    expect(screen.queryByRole("link")).toBeNull();
    category("本机工作区");
    expect(screen.getByText("DSH 本机 workspace").getAttribute("data-token")).toBe("local-token");
    category("DSH 任务");
    expect(screen.getByText("DSH 本机 tasks")).toBeTruthy();
    category("代码审查");
    expect(screen.getByText("DSH 本机 review")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("搜索设置"), { target: { value: "Skills" } });
    expect(screen.getByText("DSH 扩展管理").getAttribute("data-token")).toBe("local-token");
    fireEvent.click(screen.getByLabelText("清除搜索"));
    category("个性化");
    expect(screen.getByText(/本机全局配置已关闭学习记忆/)).toBeTruthy();
    expect(
      screen
        .getByRole("switch", { name: "在任务中使用已保存记忆" })
        .getAttribute("aria-checked"),
    ).toBe("true");
    category("用量");
    expect(screen.getByText("12")).toBeTruthy();
    expect(screen.getByText("24")).toBeTruthy();
  });

  it("refreshes the actual model and mounted inventory after a native service change", async () => {
    const state = harness();
    const updated = { ...data, model: { ...data.model, name: "verified-model" } };
    vi.mocked(api.bootstrap).mockResolvedValue(updated);
    render(<SettingsCenter harness={state} onReplay={vi.fn()} open onOpenChange={vi.fn()} />);
    await ready();
    category("模型服务");
    expect(screen.getByRole("button", { name: "保存 DSH 模型连接" }).getAttribute("data-token")).toBe("local-token");
    fireEvent.click(screen.getByRole("button", { name: "保存 DSH 模型连接" }));
    await waitFor(() => expect(state.setData).toHaveBeenCalledWith(updated));
    expect(api.bootstrap).toHaveBeenCalledOnce();
    expect(state.refreshSessions).toHaveBeenCalledOnce();
  });

  it("persists custom instructions and memory choices through the real settings patch contract", async () => {
    render(<SettingsCenter harness={harness()} onReplay={vi.fn()} open onOpenChange={vi.fn()} />);
    await ready();
    category("个性化");
    fireEvent.change(screen.getByLabelText("自定义指令"), {
      target: { value: "Answer as DSH. Use runnable examples." },
    });
    fireEvent.click(screen.getByRole("switch", { name: "在任务中使用已保存记忆" }));
    fireEvent.click(screen.getByRole("button", { name: "保存更改" }));
    await waitFor(() => expect(api.rpc).toHaveBeenCalledWith("local-token", "settings/update", {
      patch: {
        personality: "default",
        characteristics: { warmth: "default", enthusiasm: "default", headers_lists: "default", emoji: "default" },
        custom_instructions: "Answer as DSH. Use runnable examples.",
        memory_inject: false,
        memory_generate: true,
      },
    }));
  });

  it("uses installed recordings for each chosen startup step", async () => {
    render(<SettingsCenter harness={harness()} onReplay={vi.fn()} open onOpenChange={vi.fn()} />);
    await ready();
    category("语音");
    fireEvent.change(screen.getByLabelText("试听步骤"), { target: { value: "phase-3-mounted" } });
    expect(screen.getByLabelText("试听 DSH 英文开场旁白").getAttribute("src")).toBe("/assets/voice/phase-3-mounted.wav");
    fireEvent.change(screen.getByLabelText("试听步骤"), { target: { value: "load-warning" } });
    expect(screen.getByLabelText("试听 DSH 英文开场旁白").getAttribute("src")).toBe("/assets/voice/load-warning.wav");
  });

  it("keeps the profile draft and reports actual save failure instead of claiming persistence", async () => {
    const state = harness();
    vi.mocked(api.post)
      .mockRejectedValueOnce(new Error("disk write failed"))
      .mockResolvedValueOnce({ username: "New operator", badge_id: "01" });
    render(
      <SettingsCenter
        harness={state}
        onReplay={vi.fn()}
        open
        onOpenChange={vi.fn()}
      />,
    );
    await ready();
    category("个人资料");
    fireEvent.change(screen.getByLabelText("显示名称"), {
      target: { value: "New operator" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存资料" }));
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "disk write failed",
    );
    expect((screen.getByLabelText("显示名称") as HTMLInputElement).value).toBe(
      "New operator",
    );
    expect(state.setData).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "保存资料" }));
    await waitFor(() => expect(state.setData).toHaveBeenCalledOnce());
    expect(api.post).toHaveBeenLastCalledWith("/api/profile", "local-token", {
      username: "New operator",
      badge_id: "01",
    });
  });

  it("ignores a late save response after the dialog is closed and reopened", async () => {
    let resolve!: (value: unknown) => void;
    let reads = 0;
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "settings/update")
        return new Promise((done) => {
          resolve = done;
        });
      if (method === "settings/get")
        return ++reads === 1
          ? snapshot
          : {
              ...snapshot,
              effective: { ...snapshot.effective, model: "newer-model" },
            };
      return baseRpc(method);
    });
    const state = harness();
    const props = { harness: state, onReplay: vi.fn(), onOpenChange: vi.fn() };
    const view = render(<SettingsCenter {...props} open />);
    await ready();
    fireEvent.change(screen.getByLabelText("默认模型"), {
      target: { value: "old-request" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存更改" }));
    view.rerender(<SettingsCenter {...props} open={false} />);
    view.rerender(<SettingsCenter {...props} open />);
    await waitFor(() =>
      expect(
        (screen.getByLabelText("默认模型") as HTMLInputElement).value,
      ).toBe("newer-model"),
    );
    await act(async () =>
      resolve({
        ...snapshot,
        effective: { ...snapshot.effective, model: "old-request" },
      }),
    );
    expect((screen.getByLabelText("默认模型") as HTMLInputElement).value).toBe(
      "newer-model",
    );
    expect(state.setData).not.toHaveBeenCalled();
  });

  it("routes individual rename and archive operations to the selected session", async () => {
    const state = harness();
    render(
      <SettingsCenter
        harness={state}
        onReplay={vi.fn()}
        open
        onOpenChange={vi.fn()}
      />,
    );
    await ready();
    category("数据控制");
    fireEvent.click(screen.getByRole("button", { name: "管理聊天" }));
    fireEvent.click(screen.getByRole("button", { name: "重命名 真实聊天" }));
    fireEvent.change(screen.getByLabelText("聊天名称"), {
      target: { value: "更清晰的名称" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存名称" }));
    await waitFor(() => expect(state.refreshSessions).toHaveBeenCalledOnce());
    expect(api.rpc).toHaveBeenCalledWith("local-token", "sessions/rename", {
      id: "s1",
      name: "更清晰的名称",
    });
    fireEvent.click(screen.getByRole("button", { name: "归档 真实聊天" }));
    expect(api.rpc).not.toHaveBeenCalledWith(
      "local-token",
      "sessions/archive",
      { id: "s1" },
    );
    fireEvent.click(screen.getByRole("button", { name: "确认" }));
    await waitFor(() =>
      expect(api.rpc).toHaveBeenCalledWith("local-token", "sessions/archive", {
        id: "s1",
      }),
    );
  });

  it("exports only the selected snapshot using the server MIME and keeps deletion errors visible", async () => {
    const objectUrl = vi.fn().mockReturnValue("blob:fixture");
    Object.defineProperty(URL, "createObjectURL", {
      value: objectUrl,
      configurable: true,
    });
    Object.defineProperty(URL, "revokeObjectURL", {
      value: vi.fn(),
      configurable: true,
    });
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "sessions/export")
        return {
          filename: "chat.md",
          mime: "text/markdown;charset=utf-8",
          content: "# Chat",
          session_count: 1,
        };
      if (method === "sessions/delete") throw new Error("session is active");
      return baseRpc(method);
    });
    render(
      <SettingsCenter
        harness={harness()}
        onReplay={vi.fn()}
        open
        onOpenChange={vi.fn()}
      />,
    );
    await ready();
    category("数据控制");
    fireEvent.click(screen.getByRole("button", { name: "管理聊天" }));
    fireEvent.click(screen.getByRole("button", { name: "导出 真实聊天" }));
    await waitFor(() => expect(objectUrl).toHaveBeenCalledOnce());
    expect(objectUrl.mock.calls[0][0].type).toBe("text/markdown;charset=utf-8");
    expect(api.rpc).toHaveBeenCalledWith("local-token", "sessions/export", {
      format: "markdown",
      id: "s1",
    });
    fireEvent.click(screen.getByRole("button", { name: "删除 真实聊天" }));
    expect(
      screen.getByText(/这段会话的聊天快照将被永久删除/).textContent,
    ).toContain("审计事件");
    fireEvent.click(screen.getByRole("button", { name: "确认" }));
    await waitFor(() =>
      expect(
        screen
          .getAllByRole("alert")
          .some((item) => item.textContent === "session is active"),
      ).toBe(true),
    );
    expect(screen.getByRole("button", { name: "确认" })).toBeTruthy();
  });
});
