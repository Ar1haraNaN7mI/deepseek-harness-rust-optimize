import { StrictMode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { StartupGate } from "./StartupGate";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
function send(
  frame: HTMLIFrameElement,
  overrides: Record<string, unknown> = {},
) {
  const url = new URL(frame.src);
  window.dispatchEvent(
    new MessageEvent("message", {
      origin: url.origin,
      source: frame.contentWindow,
      data: {
        source: "dsh-startup",
        channel: url.searchParams.get("channel"),
        type: "complete",
      },
      ...overrides,
    }),
  );
}
describe("StartupGate", () => {
  it("renders disabled children directly and has no SSR window dependency", () => {
    expect(
      renderToString(
        <StartupGate enabled={false}>
          <p>Harness</p>
        </StartupGate>,
      ),
    ).toContain("Harness");
    expect(
      renderToString(
        <StartupGate enabled>
          <p>Harness</p>
        </StartupGate>,
      ),
    ).toContain("DSH 启动序列");
  });
  it("validates origin, source and channel before unmounting the iframe exactly once", () => {
    const finish = vi.fn();
    render(
      <StrictMode>
        <StartupGate
          enabled
          startupUrl="http://127.0.0.1:8770/startup-preview.html"
          onFinish={finish}
        >
          <p>Harness</p>
        </StartupGate>
      </StrictMode>,
    );
    const frame = screen.getByTitle("DSH 沉浸式启动动画") as HTMLIFrameElement;
    const url = new URL(frame.src);
    expect(url.searchParams.get("embed")).toBe("1");
    expect(url.searchParams.get("parentOrigin")).toBe(window.location.origin);
    act(() => send(frame, { origin: "https://example.invalid" }));
    act(() => send(frame, { source: window }));
    act(() =>
      send(frame, {
        data: { source: "dsh-startup", channel: "wrong", type: "complete" },
      }),
    );
    expect(finish).not.toHaveBeenCalled();
    act(() => {
      send(frame);
      send(frame);
    });
    expect(finish).toHaveBeenCalledExactlyOnceWith("complete");
    expect(screen.queryByTitle("DSH 沉浸式启动动画")).toBeNull();
    expect(screen.getByText("Harness")).toBeTruthy();
  });
  it("hands keyboard focus to the iframe only after a trusted ready message", () => {
    const finish = vi.fn();
    render(
      <StartupGate enabled onFinish={finish}>
        <p>App</p>
      </StartupGate>,
    );
    const frame = screen.getByTitle("DSH 沉浸式启动动画") as HTMLIFrameElement;
    const focus = vi.spyOn(frame, "focus");
    const url = new URL(frame.src);
    const ready = {
      source: "dsh-startup",
      channel: url.searchParams.get("channel"),
      type: "ready",
    };
    act(() => send(frame, { data: ready, origin: "https://example.invalid" }));
    act(() => send(frame, { data: ready, source: window }));
    act(() => send(frame, { data: { ...ready, channel: "wrong" } }));
    expect(focus).not.toHaveBeenCalled();
    act(() => send(frame, { data: ready }));
    expect(focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
    expect(document.activeElement).toBe(frame);
    expect(finish).not.toHaveBeenCalled();
  });
  it("uses the latest callback without replacing the running frame", () => {
    const first = vi.fn(),
      second = vi.fn();
    const view = render(
      <StartupGate enabled onFinish={first}>
        <p>App</p>
      </StartupGate>,
    );
    const frame = screen.getByTitle("DSH 沉浸式启动动画") as HTMLIFrameElement;
    const original = frame.src;
    view.rerender(
      <StartupGate enabled onFinish={second}>
        <p>App</p>
      </StartupGate>,
    );
    expect(frame.src).toBe(original);
    act(() => send(frame));
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledOnce();
  });
  it("offers an explicit escape when the startup endpoint never becomes ready", () => {
    vi.useFakeTimers();
    const finish = vi.fn();
    render(
      <StartupGate enabled readyTimeoutMs={100} onFinish={finish}>
        <p>App</p>
      </StartupGate>,
    );
    act(() => vi.advanceTimersByTime(101));
    fireEvent.click(screen.getByText("进入工作台"));
    expect(finish).toHaveBeenCalledWith("unavailable");
  });
  it("removes listeners on unmount and never completes from late messages", () => {
    const finish = vi.fn();
    const view = render(
      <StartupGate enabled onFinish={finish}>
        <p>App</p>
      </StartupGate>,
    );
    const frame = screen.getByTitle("DSH 沉浸式启动动画") as HTMLIFrameElement;
    const source = frame.contentWindow,
      url = new URL(frame.src);
    view.unmount();
    act(() =>
      window.dispatchEvent(
        new MessageEvent("message", {
          source,
          origin: url.origin,
          data: {
            source: "dsh-startup",
            channel: url.searchParams.get("channel"),
            type: "complete",
          },
        }),
      ),
    );
    expect(finish).not.toHaveBeenCalled();
  });
});
