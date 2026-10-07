import { afterEach, describe, expect, it, vi } from "vitest";
import { playbackDecision, readPreferences } from "./preferences";

afterEach(() => {
  localStorage.clear();
  vi.resetModules();
});
describe("web startup preferences", () => {
  it("lets explicit CLI flags override browser preferences", () => {
    expect(playbackDecision({ enabled: false, next: false }, false, true)).toBe(
      true,
    );
    expect(playbackDecision({ enabled: true, next: true }, true, false)).toBe(
      false,
    );
  });
  it("distinguishes an unset preference from an explicit false", () => {
    expect(readPreferences()).toEqual({ enabled: undefined, next: false });
    expect(playbackDecision({ next: false }, true, null)).toBe(true);
    expect(playbackDecision({ enabled: false, next: false }, true, null)).toBe(
      false,
    );
    expect(playbackDecision({ enabled: false, next: true }, false, null)).toBe(
      true,
    );
  });
  it("consumes a one-shot once while repeated StrictMode initializers agree", async () => {
    localStorage.setItem("dsh.web.startup.v1", JSON.stringify({ next: true }));
    const { consumeInitialPlayback } = await import("./preferences");
    expect(consumeInitialPlayback(false)).toBe(true);
    expect(consumeInitialPlayback(false)).toBe(true);
    expect(readPreferences().next).toBe(false);
    vi.resetModules();
    const nextPage = await import("./preferences");
    expect(nextPage.consumeInitialPlayback(false)).toBe(false);
  });
});
