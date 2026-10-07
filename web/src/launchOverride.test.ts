import { afterEach, describe, expect, it, vi } from "vitest";

const originalHref = window.location.href;
afterEach(() => {
  window.history.replaceState(null, "", originalHref);
  localStorage.clear();
  vi.resetModules();
});

describe("one-document launcher override", () => {
  it.each([
    ["on", true, false],
    ["off", false, true],
  ])(
    "honors %s over the old service and leaves saved next preference untouched",
    async (value, expected, oldServiceValue) => {
      window.history.replaceState(null, "", `/?dsh-startup=${value}`);
      const saved = JSON.stringify({ enabled: oldServiceValue, next: true });
      localStorage.setItem("dsh.web.startup.v1", saved);
      const fallback = vi.fn(() => {
        localStorage.setItem("dsh.web.startup.v1", "consumed");
        return oldServiceValue;
      });
      const { consumeLaunchPlayback } = await import("./launchOverride");
      expect(consumeLaunchPlayback(fallback)).toBe(expected);
      expect(consumeLaunchPlayback(fallback)).toBe(expected);
      expect(fallback).not.toHaveBeenCalled();
      expect(localStorage.getItem("dsh.web.startup.v1")).toBe(saved);
      expect(window.location.search).toBe("");
    },
  );

  it("removes only the launch key while preserving other query values, hash and history state", async () => {
    const state = { session: "current-session" };
    window.history.replaceState(
      state,
      "",
      "/harness?session=a%20b&dsh-startup=on&filter=skills&filter=plugins#thread",
    );
    const { consumeLaunchPlayback } = await import("./launchOverride");
    expect(consumeLaunchPlayback(() => false)).toBe(true);
    const cleaned = new URL(window.location.href);
    expect(cleaned.pathname).toBe("/harness");
    expect(cleaned.searchParams.get("session")).toBe("a b");
    expect(cleaned.searchParams.getAll("filter")).toEqual([
      "skills",
      "plugins",
    ]);
    expect(cleaned.searchParams.has("dsh-startup")).toBe(false);
    expect(cleaned.hash).toBe("#thread");
    expect(window.history.state).toEqual(state);
  });

  it("ignores and removes invalid values, then uses the existing initialization policy", async () => {
    window.history.replaceState(null, "", "/?dsh-startup=invalid&keep=yes");
    const fallback = vi.fn(() => true);
    const { consumeLaunchPlayback } = await import("./launchOverride");
    expect(consumeLaunchPlayback(fallback)).toBe(true);
    expect(fallback).toHaveBeenCalledOnce();
    expect(window.location.search).toBe("?keep=yes");
  });

  it("does not rewrite a URL without a launcher parameter", async () => {
    window.history.replaceState({ keep: true }, "", "/?session=a%20b#message");
    const replace = vi.spyOn(window.history, "replaceState");
    const { consumeLaunchPlayback } = await import("./launchOverride");
    expect(consumeLaunchPlayback(() => false)).toBe(false);
    expect(replace).not.toHaveBeenCalled();
    replace.mockRestore();
  });

  it("does not carry the consumed override into a fresh document", async () => {
    window.history.replaceState(null, "", "/?dsh-startup=on");
    const firstDocument = await import("./launchOverride");
    expect(firstDocument.consumeLaunchPlayback(() => false)).toBe(true);
    vi.resetModules();
    const nextDocument = await import("./launchOverride");
    expect(nextDocument.consumeLaunchPlayback(() => false)).toBe(false);
  });
});
