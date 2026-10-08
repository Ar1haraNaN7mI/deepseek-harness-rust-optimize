import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AccessPasswordSettings } from "./AccessPasswordSettings";
import * as api from "./api";
import type { AccessStatus } from "./types";

vi.mock("./api", async (original) => ({ ...(await original<typeof import("./api")>()), accessStatus: vi.fn(), post: vi.fn() }));
const status: AccessStatus = { enabled: false, unlocked: true, token: "token", profile: { username: "CatShark", badge_id: "DSH-0001" }, startup: { enabled: false, sound: true, reduced_motion: false } };
beforeEach(() => { vi.mocked(api.accessStatus).mockResolvedValue(status); });
afterEach(() => { cleanup(); vi.resetAllMocks(); });

describe("Access password settings", () => {
  it("starts disabled without any password and validates confirmation before saving to the backend", async () => {
    vi.mocked(api.post).mockResolvedValue({ ...status, enabled: true });
    render(<AccessPasswordSettings token="token" />);
    expect(await screen.findByText("未设置")).toBeTruthy();
    expect(screen.queryByLabelText("当前密码")).toBeNull();
    expect((screen.getByLabelText("设置密码") as HTMLInputElement).value).toBe("");
    fireEvent.change(screen.getByLabelText("设置密码"), { target: { value: "a-real-password" } });
    fireEvent.change(screen.getByLabelText("确认新密码"), { target: { value: "different" } });
    fireEvent.click(screen.getByRole("button", { name: "启用密码" }));
    expect(screen.getByRole("alert").textContent).toBe("两次输入的新密码不一致。");
    expect(api.post).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("确认新密码"), { target: { value: "a-real-password" } });
    fireEvent.click(screen.getByRole("button", { name: "启用密码" }));
    await screen.findByText("已启用");
    expect(api.post).toHaveBeenCalledExactlyOnceWith("/api/access/password", "token", { password: "a-real-password" });
    expect((screen.getByLabelText("新密码") as HTMLInputElement).value).toBe("");
    expect((screen.getByLabelText("当前密码") as HTMLInputElement).value).toBe("");
  });
  it("requires the current password for a real change and keeps failed edits available", async () => {
    vi.mocked(api.accessStatus).mockResolvedValue({ ...status, enabled: true });
    vi.mocked(api.post).mockRejectedValue(new Error("当前密码不正确"));
    render(<AccessPasswordSettings token="token" />);
    await screen.findByLabelText("当前密码");
    fireEvent.change(screen.getByLabelText("新密码"), { target: { value: "next-password" } });
    fireEvent.change(screen.getByLabelText("确认新密码"), { target: { value: "next-password" } });
    fireEvent.click(screen.getByRole("button", { name: "更新密码" }));
    expect(api.post).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("当前密码"), { target: { value: "wrong-password" } });
    fireEvent.click(screen.getByRole("button", { name: "更新密码" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toBe("当前密码不正确"));
    expect(api.post).toHaveBeenCalledExactlyOnceWith("/api/access/password", "token", { current_password: "wrong-password", password: "next-password" });
    expect((screen.getByLabelText("新密码") as HTMLInputElement).value).toBe("next-password");
  });
  it("removes the password only through the authenticated backend contract", async () => {
    vi.mocked(api.accessStatus).mockResolvedValue({ ...status, enabled: true });
    vi.mocked(api.post).mockResolvedValue(status);
    render(<AccessPasswordSettings token="token" />);
    await screen.findByLabelText("当前密码");
    fireEvent.click(screen.getByRole("button", { name: "移除密码" }));
    expect(api.post).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("当前密码"), { target: { value: "current-password" } });
    fireEvent.click(screen.getByRole("button", { name: "移除密码" }));
    expect(await screen.findByText("访问密码已移除。")).toBeTruthy();
    expect(api.post).toHaveBeenCalledExactlyOnceWith("/api/access/password", "token", { current_password: "current-password", password: "" });
    expect(screen.queryByLabelText("当前密码")).toBeNull();
  });
  it("locks the service before asking the mounted workbench to close", async () => {
    const locked = vi.fn();
    window.addEventListener("dsh-access-locked", locked);
    vi.mocked(api.accessStatus).mockResolvedValue({ ...status, enabled: true });
    vi.mocked(api.post).mockResolvedValue({ ...status, enabled: true, unlocked: false });
    render(<AccessPasswordSettings token="token" />);
    fireEvent.click(await screen.findByRole("button", { name: "立即锁定" }));
    await waitFor(() => expect(locked).toHaveBeenCalledOnce());
    expect(api.post).toHaveBeenCalledExactlyOnceWith("/api/access/lock", "token", {});
    window.removeEventListener("dsh-access-locked", locked);
  });
});
