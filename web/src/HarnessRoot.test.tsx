import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HarnessRoot } from "./HarnessRoot";
import * as api from "./api";
import { consumeInitialPlayback } from "./preferences";
import type { AccessStatus, Bootstrap } from "./types";

vi.mock("./App", () => ({ App: () => <div>Private workbench</div> }));
vi.mock("./Emblem", () => ({ Emblem: () => <span>DSH</span> }));
vi.mock("./api", async (original) => ({ ...(await original<typeof import("./api")>()), accessStatus: vi.fn(), bootstrap: vi.fn(), post: vi.fn() }));
vi.mock("./preferences", () => ({ consumeInitialPlayback: vi.fn() }));
vi.mock("./launchOverride", () => ({ consumeLaunchPlayback: (fallback: () => boolean) => fallback() }));
const status: AccessStatus = { enabled: true, unlocked: false, token: "csrf-token", profile: { username: "CatShark", badge_id: "DSH-0001" }, startup: { enabled: true, reduced_motion: false, sound: true } };
const boot = { token: "csrf-token", profile: status.profile } as Bootstrap;

function send(frame: HTMLIFrameElement, type: string) {
  const url = new URL(frame.src);
  window.dispatchEvent(new MessageEvent("message", {
    origin: url.origin, source: frame.contentWindow,
    data: { source: "dsh-startup", channel: url.searchParams.get("channel"), type },
  }));
}
beforeEach(() => {
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
  vi.mocked(api.accessStatus).mockResolvedValue(status);
  vi.mocked(api.bootstrap).mockResolvedValue(boot);
  vi.mocked(consumeInitialPlayback).mockReturnValue(false);
});
afterEach(() => { cleanup(); vi.resetAllMocks(); vi.unstubAllGlobals(); });

describe("Access-gated Harness", () => {
  it("reports a usable locked native entry without fetching private bootstrap", async () => {
    const postMessage = vi.fn();
    vi.stubGlobal("__DSH_DESKTOP__", true);
    vi.stubGlobal("ipc", { postMessage });
    render(<HarnessRoot />);
    await screen.findByRole("heading", { name: "访问身份确认" });
    expect(postMessage).toHaveBeenCalledWith(JSON.stringify({ event: "dsh-ready", version: 1 }));
    expect(api.bootstrap).not.toHaveBeenCalled();
  });
  it("does not request bootstrap or mount the app when animation is disabled and access is locked", async () => {
    render(<HarnessRoot />);
    expect(await screen.findByRole("heading", { name: "访问身份确认" })).toBeTruthy();
    expect(screen.getByText("CatShark")).toBeTruthy();
    expect(screen.getByText("DSH-0001")).toBeTruthy();
    expect(screen.getByLabelText("输入密码").getAttribute("type")).toBe("password");
    expect(api.bootstrap).not.toHaveBeenCalled();
    expect(screen.queryByText("Private workbench")).toBeNull();
  });
  it("shows real authentication errors and only requests private bootstrap after backend re-verification", async () => {
    vi.mocked(api.post).mockRejectedValueOnce(new Error("密码不正确")).mockResolvedValueOnce({ ...status, unlocked: true });
    render(<HarnessRoot />);
    const field = await screen.findByLabelText("输入密码");
    fireEvent.change(field, { target: { value: "wrong-password" } });
    fireEvent.click(screen.getByRole("button", { name: "验证并进入" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "密码不正确");
    expect(api.bootstrap).not.toHaveBeenCalled();
    vi.mocked(api.accessStatus).mockResolvedValue({ ...status, unlocked: true });
    fireEvent.change(field, { target: { value: "correct-password" } });
    fireEvent.click(screen.getByRole("button", { name: "验证并进入" }));
    expect(await screen.findByText("Private workbench")).toBeTruthy();
    expect(api.post).toHaveBeenLastCalledWith("/api/access/unlock", "csrf-token", { password: "correct-password" });
    expect(api.accessStatus).toHaveBeenCalledTimes(2);
    expect(api.bootstrap).toHaveBeenCalledOnce();
    expect(localStorage.length).toBe(0);
  });
  it("cannot bypass authentication by skipping the film or sending an unlocked message", async () => {
    vi.mocked(consumeInitialPlayback).mockReturnValue(true);
    render(<HarnessRoot />);
    const frame = await screen.findByTitle("DSH 沉浸式启动动画") as HTMLIFrameElement;
    await act(async () => send(frame, "unlocked"));
    expect(api.accessStatus).toHaveBeenCalledTimes(2);
    expect(api.bootstrap).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "跳过开场" }));
    await waitFor(() => expect(api.accessStatus).toHaveBeenCalledTimes(3));
    expect(screen.getByRole("heading", { name: "访问身份确认" })).toBeTruthy();
    expect(api.bootstrap).not.toHaveBeenCalled();
    expect(screen.queryByText("Private workbench")).toBeNull();
  });
  it("shows a failed post-unlock recheck and retries without requiring the password again", async () => {
    vi.mocked(api.accessStatus).mockResolvedValueOnce(status).mockRejectedValueOnce(new Error("访问状态读取失败"));
    vi.mocked(api.post).mockResolvedValue({ ...status, unlocked: true });
    render(<HarnessRoot />);
    const field = await screen.findByLabelText("输入密码");
    fireEvent.change(field, { target: { value: "correct-password" } });
    fireEvent.click(screen.getByRole("button", { name: "验证并进入" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "访问状态读取失败");
    expect((field as HTMLInputElement).value).toBe("");
    expect(api.bootstrap).not.toHaveBeenCalled();
    vi.mocked(api.accessStatus).mockResolvedValue({ ...status, unlocked: true });
    fireEvent.click(screen.getByRole("button", { name: "重新检查访问状态" }));
    expect(await screen.findByText("Private workbench")).toBeTruthy();
    expect(api.post).toHaveBeenCalledOnce();
  });
  it("exposes iframe unlock recheck failures instead of hiding them behind the film", async () => {
    vi.mocked(consumeInitialPlayback).mockReturnValue(true);
    render(<HarnessRoot />);
    const frame = await screen.findByTitle("DSH 沉浸式启动动画") as HTMLIFrameElement;
    vi.mocked(api.accessStatus).mockRejectedValue(new Error("访问服务暂时离线"));
    await act(async () => send(frame, "unlocked"));
    expect(screen.queryByTitle("DSH 沉浸式启动动画")).toBeNull();
    expect(screen.getByRole("alert").textContent).toBe("访问服务暂时离线");
    expect(screen.getByRole("button", { name: "重新检查访问状态" })).toBeTruthy();
    expect(api.bootstrap).not.toHaveBeenCalled();
  });
  it("keeps the same film iframe alive when a verified unlock loads the actual app", async () => {
    vi.mocked(consumeInitialPlayback).mockReturnValue(true);
    render(<HarnessRoot />);
    const frame = await screen.findByTitle("DSH 沉浸式启动动画") as HTMLIFrameElement;
    vi.mocked(api.accessStatus).mockResolvedValue({ ...status, unlocked: true });
    await act(async () => send(frame, "unlocked"));
    await waitFor(() => expect(api.bootstrap).toHaveBeenCalledOnce());
    expect(screen.getByTitle("DSH 沉浸式启动动画")).toBe(frame);
    expect(screen.getByText("Private workbench").closest("[hidden]")).toBeTruthy();
    act(() => send(frame, "complete"));
    expect(screen.getByText("Private workbench").closest("[hidden]")).toBeNull();
  });
  it("removes already-rendered conversations when the service revokes access", async () => {
    vi.mocked(api.accessStatus).mockResolvedValue({ ...status, unlocked: true });
    render(<HarnessRoot />);
    expect(await screen.findByText("Private workbench")).toBeTruthy();
    vi.mocked(api.accessStatus).mockResolvedValue(status);
    await act(async () => window.dispatchEvent(new Event("dsh-access-locked")));
    expect(await screen.findByLabelText("输入密码")).toBeTruthy();
    expect(screen.queryByText("Private workbench")).toBeNull();
    expect(api.bootstrap).toHaveBeenCalledOnce();
  });
  it("does not fail open when access status cannot be read", async () => {
    vi.mocked(api.accessStatus).mockRejectedValue(new Error("offline"));
    render(<HarnessRoot />);
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", expect.stringContaining("offline"));
    expect(api.bootstrap).not.toHaveBeenCalled();
    expect(screen.queryByText("Private workbench")).toBeNull();
  });
});
