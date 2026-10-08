import { afterEach, describe, expect, it, vi } from "vitest";
import { accessStatus } from "./api";

const valid = {
  enabled: true, unlocked: false, token: "csrf-token",
  profile: { username: "Operator", badge_id: "DSH-01" },
  startup: { enabled: true, override_enabled: null, reduced_motion: false, sound: true },
};
afterEach(() => vi.unstubAllGlobals());

describe("access status response", () => {
  it("accepts the explicit boolean status and only public identity metadata", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, status: 200, json: async () => valid }));
    expect(await accessStatus()).toEqual(valid);
  });
  it.each([
    null,
    [],
    {},
    { ...valid, unlocked: "true" },
    { ...valid, enabled: "false", unlocked: true },
    { ...valid, token: "" },
    { ...valid, profile: null },
    { ...valid, profile: { username: 123, badge_id: "DSH-01" } },
    { ...valid, startup: { ...valid.startup, enabled: "false" } },
    { ...valid, startup: { ...valid.startup, override_enabled: "on" } },
  ])("rejects malformed access state instead of treating truthy values as unlocked (%j)", async (value) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, status: 200, json: async () => value }));
    await expect(accessStatus()).rejects.toThrow("本地服务返回了无效的访问状态");
  });
});
