import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ExtensionSettings } from "./ExtensionSettings";
import * as api from "./api";

vi.mock("./api", async (original) => ({ ...(await original<typeof import("./api")>()), rpc: vi.fn() }));
const plugin = { id: "echo", name: "Echo", version: "1.0", description: "Local echo plugin", root: "C:/DSH/plugins/echo", enabled: true, mounted: true, managed: true, tools: ["echo"], skills: ["skills/helper/SKILL.md"] };
const skill = { name: "helper", description: "Read actual instructions", path: "C:/DSH/plugins/echo/skills/helper/SKILL.md", source: "plugin", enabled: true };
const snapshot = { plugins: [plugin], skills: [skill], install_root: "C:/DSH/plugins" };

beforeEach(() => vi.mocked(api.rpc).mockReset());
afterEach(cleanup);

describe("DSH extension management", () => {
  it("waits for persisted server state before displaying a disabled plugin", async () => {
    let commit!: (value: typeof snapshot) => void;
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "plugins/disable") return new Promise((resolve) => { commit = resolve as typeof commit; });
      return snapshot;
    });
    const changed = vi.fn();
    render(<ExtensionSettings token="token" onChanged={changed} />);
    fireEvent.click(await screen.findByRole("button", { name: "停用" }));
    expect(screen.getByText("已加载")).toBeTruthy();
    expect(changed).not.toHaveBeenCalled();
    commit({ ...snapshot, plugins: [{ ...plugin, enabled: false, mounted: false }], skills: [] });
    await screen.findByText("已停用");
    expect(api.rpc).toHaveBeenCalledWith("token", "plugins/disable", { id: "echo" });
    expect(changed).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: "Skills (0)" })).toBeTruthy();
  });

  it("surfaces failed persistence and keeps actual activation state", async () => {
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => {
      if (method === "plugins/disable") throw new Error("activation file cannot be written");
      return snapshot;
    });
    const changed = vi.fn();
    render(<ExtensionSettings token="token" onChanged={changed} />);
    fireEvent.click(await screen.findByRole("button", { name: "停用" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "activation file cannot be written");
    await waitFor(() => expect((screen.getByRole("button", { name: "停用" }) as HTMLButtonElement).disabled).toBe(false));
    expect(screen.getByText("已加载")).toBeTruthy();
    expect(changed).not.toHaveBeenCalled();
  });

  it("uninstalls only after confirmation and refreshes from the returned registry", async () => {
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => method === "plugins/uninstall" ? { ...snapshot, plugins: [], skills: [] } : snapshot);
    render(<ExtensionSettings token="token" />);
    fireEvent.click(await screen.findByRole("button", { name: "卸载" }));
    const confirmation = screen.getByRole("group", { name: "确认卸载 Echo" });
    expect(vi.mocked(api.rpc).mock.calls.some(([, method]) => method === "plugins/uninstall")).toBe(false);
    fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("group", { name: "确认卸载 Echo" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "卸载" }));
    fireEvent.click(screen.getByRole("button", { name: "确认卸载" }));
    await screen.findByText("没有匹配的本机插件。");
    expect(api.rpc).toHaveBeenCalledWith("token", "plugins/uninstall", { id: "echo" });
  });

  it("sends the selected local install path and reads actual skill instructions", async () => {
    vi.mocked(api.rpc).mockImplementation(async (_token, method) => method === "skills/read" ? { name: "helper", content: "Actual DSH instructions from disk." } : snapshot);
    render(<ExtensionSettings token="token" />);
    const input = await screen.findByRole("textbox", { name: "从本机目录安装或更新" });
    fireEvent.change(input, { target: { value: "C:/Packages/echo" } });
    fireEvent.click(screen.getByRole("button", { name: "安装并加载" }));
    await screen.findByText("插件已安装并加载。相同 ID 的安装会更新原插件。");
    expect(api.rpc).toHaveBeenCalledWith("token", "plugins/install", { path: "C:/Packages/echo" });
    fireEvent.click(screen.getByRole("button", { name: "Skills (1)" }));
    fireEvent.click(screen.getByRole("button", { name: "查看指令" }));
    expect(await screen.findByText("Actual DSH instructions from disk.")).toBeTruthy();
    expect(api.rpc).toHaveBeenCalledWith("token", "skills/read", { name: "helper" });
  });

  it("does not offer deletion for external plugin sources", async () => {
    vi.mocked(api.rpc).mockResolvedValue({ ...snapshot, plugins: [{ ...plugin, managed: false }] });
    render(<ExtensionSettings token="token" />);
    await screen.findByText("Echo");
    expect(screen.queryByRole("button", { name: "卸载" })).toBeNull();
    expect(screen.getByRole("button", { name: "停用" })).toBeTruthy();
  });
});
