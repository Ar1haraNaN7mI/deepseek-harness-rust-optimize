import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ComputerSettings } from "./ComputerSettings";
import * as api from "./api";

vi.mock("./api", async (original) => ({ ...(await original<typeof import("./api")>()), rpc: vi.fn() }));
const available = { enabled: true, supported: true, native: true, model_input: "uia_text", tools: ["computer_windows", "computer_observe", "computer_act"] };
const rect = { x: 10, y: 10, width: 800, height: 600 };
const windows = [
  { window_id: "window-editor", title: "Fixture editor", pid: 101, rect, foreground: true },
  { window_id: "window-other", title: "Other fixture", pid: 102, rect, foreground: false },
];
const snapshot = {
  window_id: "window-editor", snapshot_id: "frame-1", rect, truncated: false, expires_in_ms: 60000,
  nodes: [
    { node_id: "save", role: "Button", name: "Save fixture", enabled: true, offscreen: false, password: false, patterns: ["Invoke"] },
    { node_id: "text", role: "Edit", name: "Document text", value: "Real UI text fixture", enabled: true, offscreen: false, password: false, patterns: ["set_value", "type_text", "key", "scroll"] },
    { node_id: "hidden", role: "Button", name: "Hidden fixture", enabled: true, offscreen: true, password: false, patterns: ["Invoke"] },
    { node_id: "disabled", role: "Button", name: "Disabled fixture", enabled: false, offscreen: false, password: false, patterns: ["Invoke"] },
  ],
  pointer: { x: 120, y: 80, visible: true },
  screenshot: { data_url: "data:image/png;base64,aGVsbG8=", mime: "image/png", width: 800, height: 600 },
};
let currentStatus = available;
let observations = 0;
async function response(_token: string, method: string, params?: Record<string, unknown>) {
  if (method === "computer/status") return currentStatus;
  if (method === "computer/setEnabled") return { ...available, enabled: params?.enabled };
  if (method === "computer/windows") return { windows };
  if (method === "computer/observe") return { ...snapshot, snapshot_id: `frame-${++observations}`, window_id: params?.window_id };
  if (method === "computer/act") return { ...snapshot, performed: params?.action };
  throw new Error(`Unexpected fixture method ${method}`);
}
beforeEach(() => {
  observations = 0;
  currentStatus = available;
  vi.mocked(api.rpc).mockImplementation(response);
});
afterEach(() => { cleanup(); vi.resetAllMocks(); vi.useRealTimers(); });
async function chooseWindow() {
  await screen.findByRole("option", { name: "Fixture editor · 当前前台" });
  await waitFor(() => expect((screen.getByLabelText("目标窗口") as HTMLSelectElement).disabled).toBe(false));
  fireEvent.change(screen.getByLabelText("目标窗口"), { target: { value: "window-editor" } });
}
async function observe() {
  await chooseWindow();
  fireEvent.click(screen.getByRole("button", { name: "读取画面与控件" }));
  await screen.findByRole("img", { name: "Fixture editor 当前窗口画面" });
  await waitFor(() => expect((screen.getByLabelText("目标控件") as HTMLSelectElement).disabled).toBe(false));
}
function calls(method: string) { return vi.mocked(api.rpc).mock.calls.filter((call) => call[1] === method); }

describe("Computer use settings", () => {
  it("does not enumerate windows until the real persisted switch is enabled", async () => {
    currentStatus = { ...available, enabled: false };
    const changed = vi.fn();
    render(<ComputerSettings token="token" onChanged={changed} />);
    expect(await screen.findByText("Windows 本机窗口 · 已关闭")).toBeTruthy();
    expect(screen.getByRole("switch", { name: "启用电脑操作" }).getAttribute("aria-checked")).toBe("false");
    expect(calls("computer/windows")).toHaveLength(0);
    fireEvent.click(screen.getByRole("switch", { name: "启用电脑操作" }));
    await screen.findByRole("option", { name: "Fixture editor · 当前前台" });
    expect(calls("computer/setEnabled")[0][2]).toEqual({ enabled: true });
    expect(changed).toHaveBeenCalledOnce();
    expect(calls("computer/observe")).toHaveLength(0);
  });
  it("reports unsupported platforms and keeps all computer controls off", async () => {
    currentStatus = { ...available, enabled: false, supported: false, reason: "当前系统不支持 Windows 窗口操作" } as typeof available;
    render(<ComputerSettings token="token" />);
    expect(await screen.findByText("当前系统不支持 Windows 窗口操作")).toBeTruthy();
    expect((screen.getByRole("switch", { name: "启用电脑操作" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.queryByLabelText("目标窗口")).toBeNull();
    expect(calls("computer/windows")).toHaveLength(0);
  });
  it("renders the actual screenshot and readable controls without sending them to a vision API", async () => {
    render(<ComputerSettings token="token" />);
    await observe();
    expect(screen.getByRole("img", { name: "Fixture editor 当前窗口画面" }).getAttribute("src")).toBe(snapshot.screenshot.data_url);
    fireEvent.click(screen.getByText("窗口文字与控件"));
    expect(screen.getByText(/Real UI text fixture/)).toBeTruthy();
    expect(screen.queryByRole("option", { name: "Hidden fixture · 按钮" })).toBeNull();
    expect((screen.getByRole("option", { name: "Disabled fixture · 按钮" }) as HTMLOptionElement).disabled).toBe(true);
    expect(calls("computer/act")).toHaveLength(0);
    expect(calls("computer/observe")[0][2]).toEqual({ window_id: "window-editor" });
    const pointer = screen.getByRole("img", { name: "DSH 独立指针：120, 80" });
    expect(pointer.style.left).toBe("15%");
    expect(parseFloat(pointer.style.top)).toBeCloseTo(13.333, 2);
  });
  it("invokes only the selected supported control with the current snapshot then refreshes", async () => {
    render(<ComputerSettings token="token" />);
    await observe();
    expect((screen.getByRole("button", { name: "点击所选控件" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "save" } });
    fireEvent.click(screen.getByRole("button", { name: "点击所选控件" }));
    expect(await screen.findByText("操作已完成，窗口画面已更新。")).toBeTruthy();
    expect(calls("computer/act")[0][2]).toEqual({ window_id: "window-editor", snapshot_id: "frame-1", action: "invoke", node_id: "save" });
    expect(calls("computer/observe")).toHaveLength(2);
    expect((screen.getByLabelText("目标控件") as HTMLSelectElement).value).toBe("");
  });
  it("sends text to a real editable control and uses fresh snapshots for keyboard and scrolling", async () => {
    render(<ComputerSettings token="token" />);
    await observe();
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "text" } });
    fireEvent.change(screen.getByLabelText("填写内容"), { target: { value: "Entered fixture text" } });
    fireEvent.click(screen.getByRole("button", { name: "填写所选控件" }));
    await screen.findByText("操作已完成，窗口画面已更新。");
    expect(calls("computer/act")[0][2]).toEqual({ window_id: "window-editor", snapshot_id: "frame-1", action: "set_value", node_id: "text", text: "Entered fixture text" });
    expect((screen.getByLabelText("填写内容") as HTMLTextAreaElement).value).toBe("");
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "text" } });
    fireEvent.change(screen.getByLabelText("按键"), { target: { value: "CTRL+A" } });
    fireEvent.click(screen.getByRole("button", { name: "发送按键" }));
    await waitFor(() => expect(calls("computer/observe")).toHaveLength(3));
    expect(calls("computer/act")[1][2]).toEqual({ window_id: "window-editor", snapshot_id: "frame-2", action: "key", node_id: "text", key: "CTRL+A" });
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "text" } });
    fireEvent.click(screen.getByRole("button", { name: "向下滚动" }));
    await waitFor(() => expect(calls("computer/observe")).toHaveLength(4));
    expect(calls("computer/act")[2][2]).toEqual({ window_id: "window-editor", snapshot_id: "frame-3", action: "scroll", node_id: "text", delta: -3 });
  });
  it("requires a supported explicit control for background typing, keys and scrolling", async () => {
    render(<ComputerSettings token="token" />);
    await observe();
    fireEvent.change(screen.getByLabelText("填写内容"), { target: { value: "Background input" } });
    for (const name of ["后台键入所选控件", "发送按键", "向下滚动"]) expect((screen.getByRole("button", { name }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "save" } });
    fireEvent.change(screen.getByLabelText("填写内容"), { target: { value: "Background input" } });
    for (const name of ["后台键入所选控件", "发送按键", "向下滚动"]) expect((screen.getByRole("button", { name }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "text" } });
    fireEvent.change(screen.getByLabelText("填写内容"), { target: { value: "Background input" } });
    fireEvent.click(screen.getByRole("button", { name: "后台键入所选控件" }));
    await screen.findByText("操作已完成，窗口画面已更新。");
    expect(calls("computer/act")[0][2]).toEqual({ action: "type_text", window_id: "window-editor", snapshot_id: "frame-1", node_id: "text", text: "Background input" });
  });
  it("updates the independent cursor from native observation without any system pointer call", async () => {
    let performed = false;
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => {
      if (method === "computer/act") { performed = true; return { performed: params?.action, pointer: { x: 300, y: 180, visible: true } }; }
      if (method === "computer/observe" && performed) return { ...snapshot, snapshot_id: "after-action", pointer: { x: 300, y: 180, visible: true } };
      return response(token, method, params);
    });
    render(<ComputerSettings token="token" />);
    await observe();
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "save" } });
    fireEvent.click(screen.getByRole("button", { name: "点击所选控件" }));
    await screen.findByText("操作已完成，窗口画面已更新。");
    const pointer = screen.getByRole("img", { name: "DSH 独立指针：300, 180" });
    expect(pointer.style.left).toBe("37.5%");
    expect(pointer.style.top).toBe("30%");
    expect(calls("computer/act")).toHaveLength(1);
    expect(calls("computer/act")[0][2]?.action).toBe("invoke");
  });
  it("invalidates expired observations and never submits a stale operation", async () => {
    render(<ComputerSettings token="token" />);
    await chooseWindow();
    vi.useFakeTimers();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "读取画面与控件" })));
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "save" } });
    expect((screen.getByRole("button", { name: "点击所选控件" }) as HTMLButtonElement).disabled).toBe(false);
    await act(async () => vi.advanceTimersByTime(55001));
    expect(screen.getByText("请刷新画面后操作")).toBeTruthy();
    expect((screen.getByRole("button", { name: "点击所选控件" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "点击所选控件" }));
    expect(calls("computer/act")).toHaveLength(0);
  });
  it("preserves a real mutation failure and disables the consumed snapshot until refreshed", async () => {
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => {
      if (method === "computer/act") throw new Error("窗口已经移动，请重新观察");
      return response(token, method, params);
    });
    render(<ComputerSettings token="token" />);
    await observe();
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "text" } });
    fireEvent.click(screen.getByRole("button", { name: "发送按键" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "窗口已经移动，请重新观察");
    expect((screen.getByRole("button", { name: "发送按键" }) as HTMLButtonElement).disabled).toBe(true);
    expect(calls("computer/observe")).toHaveLength(1);
    expect(screen.queryByText("操作已完成，窗口画面已更新。")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "刷新画面与控件" }));
    await waitFor(() => expect((screen.getByLabelText("目标控件") as HTMLSelectElement).disabled).toBe(false));
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "text" } });
    expect((screen.getByRole("button", { name: "发送按键" }) as HTMLButtonElement).disabled).toBe(false);
  });
  it("reports a performed close action as success and removes the vanished target", async () => {
    let closed = false;
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => {
      if (method === "computer/act") { closed = true; return { performed: params?.action, window_id: "window-editor", refresh_required: true, observation_error: "Window closed" }; }
      if (method === "computer/windows" && closed) return { windows: [windows[1]] };
      return response(token, method, params);
    });
    render(<ComputerSettings token="token" />);
    await observe();
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "save" } });
    fireEvent.click(screen.getByRole("button", { name: "点击所选控件" }));
    expect(await screen.findByText("操作已完成，目标窗口已关闭或不再可见。")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("img")).toBeNull();
    expect((screen.getByLabelText("目标窗口") as HTMLSelectElement).value).toBe("");
    expect(calls("computer/observe")).toHaveLength(1);
  });
  it("distinguishes a successful action from a failed follow-up observation", async () => {
    let performed = false;
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => {
      if (method === "computer/act") { performed = true; return { performed: params?.action }; }
      if (method === "computer/observe" && performed) throw new Error("控件读取超时");
      return response(token, method, params);
    });
    render(<ComputerSettings token="token" />);
    await observe();
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "text" } });
    fireEvent.click(screen.getByRole("button", { name: "发送按键" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "操作已执行，但读取新画面失败：控件读取超时。请刷新窗口后重新读取。");
    expect(screen.queryByText("操作已完成，正在读取新的窗口画面。")).toBeNull();
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.getByRole("button", { name: "读取画面与控件" })).toBeTruthy();
    expect(calls("computer/act")).toHaveLength(1);
  });
  it("does not offer or send text input for a password control", async () => {
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => method === "computer/observe" ? {
      ...snapshot,
      nodes: [...snapshot.nodes, { node_id: "secret", role: "Edit", name: "[password field]", value: null, enabled: true, offscreen: false, password: true, patterns: [], background_patterns: ["type_text", "key", "scroll"] }],
    } : response(token, method, params));
    render(<ComputerSettings token="token" />);
    await observe();
    fireEvent.change(screen.getByLabelText("填写内容"), { target: { value: "discard this draft" } });
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "secret" } });
    expect(screen.getByText("密码请直接在目标窗口中手动输入。")).toBeTruthy();
    expect((screen.getByLabelText("填写内容") as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByLabelText("填写内容") as HTMLInputElement).value).toBe("");
    expect((screen.getByRole("button", { name: "填写所选控件" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "后台键入所选控件" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "后台键入所选控件" }));
    fireEvent.click(screen.getByRole("button", { name: "填写所选控件" }));
    expect(calls("computer/act")).toHaveLength(0);
  });
  it("keeps real accessible text available when screenshot capture fails", async () => {
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => method === "computer/observe" ? { ...snapshot, screenshot: null, screenshot_error: "这个窗口不允许读取图像" } : response(token, method, params));
    render(<ComputerSettings token="token" />);
    await chooseWindow();
    fireEvent.click(screen.getByRole("button", { name: "读取画面与控件" }));
    expect(await screen.findByText("这个窗口不允许读取图像")).toBeTruthy();
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.getByText(/Real UI text fixture/)).toBeTruthy();
  });
  it("shows real local OCR content and original window positions as plain text", async () => {
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => method === "computer/observe" ? {
      ...snapshot, recognition: { status: "ok", language: "zh-Hans-CN", text: "DSH OPERATOR 4729 <script>text only</script>", lines: [{ text: "DSH OPERATOR", bounds: { x: 24, y: 42, width: 300, height: 40 }, words: [] }] },
    } : response(token, method, params));
    render(<ComputerSettings token="token" />);
    await observe();
    expect(screen.getByText("DSH OPERATOR 4729 <script>text only</script>")).toBeTruthy();
    expect(document.querySelector(".cu-recognition script")).toBeNull();
    expect(screen.getByText(/24, 42 · 300 × 40/)).toBeTruthy();
    expect(screen.getByText(/zh-Hans-CN · 模型可读取这些文字与位置/)).toBeTruthy();
  });
  it("reports an actual OCR failure while preserving the screenshot and readable controls", async () => {
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => method === "computer/observe" ? {
      ...snapshot, recognition: { status: "unavailable", text: "", lines: [], error: "本机没有可用的 Windows OCR 语言" },
    } : response(token, method, params));
    render(<ComputerSettings token="token" />);
    await observe();
    expect(screen.getByText("本机没有可用的 Windows OCR 语言")).toBeTruthy();
    expect(screen.getByText(/Real UI text fixture/)).toBeTruthy();
    expect(screen.getByRole("img", { name: "Fixture editor 当前窗口画面" })).toBeTruthy();
  });
  it("offers declared background input and reports delivery without claiming the app accepted it", async () => {
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => {
      if (method === "computer/observe") return { ...snapshot, nodes: [...snapshot.nodes, { node_id: "canvas", role: "Pane", name: "Custom surface", enabled: true, offscreen: false, password: false, patterns: [], background_patterns: ["type_text", "key", "scroll"] }] };
      if (method === "computer/act") return { performed: "background_messages_sent", delivery: "unverified" };
      return response(token, method, params);
    });
    render(<ComputerSettings token="token" />);
    await observe();
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "canvas" } });
    expect((screen.getByRole("button", { name: "填写所选控件" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "发送按键" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("填写内容"), { target: { value: "Background fixture" } });
    fireEvent.click(screen.getByRole("button", { name: "后台键入所选控件" }));
    await screen.findByText("后台操作已发送，画面已更新；请核对目标应用是否响应。");
    expect(calls("computer/act")[0][2]).toEqual({ action: "type_text", window_id: "window-editor", snapshot_id: "frame-1", node_id: "canvas", text: "Background fixture" });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText("操作已完成，窗口画面已更新。")).toBeNull();
  });
  it("maps a preview click to physical window pixels and uses the returned background input target", async () => {
    let clicked = false;
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => {
      if (method === "computer/act") { clicked = true; return { performed: "background_messages_sent", delivery: "unverified" }; }
      if (method === "computer/observe" && clicked) return { ...snapshot, snapshot_id: "after-click", input_target: { node_id: "background", window_id: "window-editor", class: "FixtureCanvas", bounds: rect, delivery: "unverified" } };
      return response(token, method, params);
    });
    render(<ComputerSettings token="token" />);
    await observe();
    const image = screen.getByRole("img", { name: "Fixture editor 当前窗口画面" });
    vi.spyOn(image, "getBoundingClientRect").mockReturnValue({ left: 20, top: 30, width: 400, height: 300, right: 420, bottom: 330, x: 20, y: 30, toJSON: () => ({}) });
    fireEvent.click(image, { clientX: 170, clientY: 120 });
    await screen.findByText("后台操作已发送，画面已更新；请核对目标应用是否响应。");
    expect(calls("computer/act")[0][2]).toEqual({ action: "click", window_id: "window-editor", snapshot_id: "frame-1", x: 300, y: 180, button: "left" });
    fireEvent.change(screen.getByLabelText("目标控件"), { target: { value: "background" } });
    fireEvent.change(screen.getByLabelText("按键"), { target: { value: "Home" } });
    fireEvent.click(screen.getByRole("button", { name: "发送按键" }));
    await waitFor(() => expect(calls("computer/act")).toHaveLength(2));
    expect(calls("computer/act")[1][2]).toEqual({ action: "key", window_id: "window-editor", snapshot_id: "after-click", node_id: "background", key: "Home" });
  });
  it("clears the previous observation when changing the target or disabling the service", async () => {
    render(<ComputerSettings token="token" />);
    await observe();
    fireEvent.change(screen.getByLabelText("目标窗口"), { target: { value: "window-other" } });
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.queryByRole("button", { name: "发送按键" })).toBeNull();
    fireEvent.click(screen.getByRole("switch", { name: "启用电脑操作" }));
    await screen.findByText("Windows 本机窗口 · 已关闭");
    expect(calls("computer/setEnabled")[0][2]).toEqual({ enabled: false });
    expect(screen.queryByLabelText("目标窗口")).toBeNull();
    expect(calls("computer/act")).toHaveLength(0);
  });
  it("discards a pending observation after switching to another runtime", async () => {
    let resolve!: (value: unknown) => void;
    vi.mocked(api.rpc).mockImplementation(async (token, method, params) => method === "computer/observe" ? new Promise((done) => { resolve = done; }) : response(token, method, params));
    const view = render(<ComputerSettings token="old-token" />);
    await chooseWindow();
    fireEvent.click(screen.getByRole("button", { name: "读取画面与控件" }));
    view.rerender(<ComputerSettings token="new-token" />);
    await screen.findByRole("option", { name: "Fixture editor · 当前前台" });
    await act(async () => resolve(snapshot));
    expect(screen.queryByRole("img")).toBeNull();
    expect((screen.getByLabelText("目标窗口") as HTMLSelectElement).value).toBe("");
  });
});
