import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Companion } from "./Companion";

beforeEach(() => vi.useFakeTimers());
afterEach(() => { cleanup(); vi.useRealTimers(); });

it("greets, returns to idle, reacts to hover and follows real working state", () => {
  const view = render(<Companion working={false} />);
  const pet = () => view.container.querySelector(".companion")!;
  expect(pet().getAttribute("data-motion")).toBe("waving");
  act(() => vi.advanceTimersByTime(2200));
  expect(pet().getAttribute("data-motion")).toBe("idle");
  fireEvent.pointerEnter(screen.getByRole("button"));
  expect(pet().getAttribute("data-motion")).toBe("curious");
  view.rerender(<Companion working />);
  expect(pet().getAttribute("data-motion")).toBe("working");
  fireEvent.click(screen.getByRole("button"));
  expect(screen.getByRole("status").textContent).toContain("这次结果");
  expect(pet().getAttribute("data-motion")).toBe("working");
  act(() => vi.advanceTimersByTime(3600));
  expect(screen.queryByRole("status")).toBeNull();
});

it("waits for the durable outcome and celebrates success rather than a cancellation", () => {
  const view = render(<Companion working preview />);
  const motion = () => view.container.querySelector(".companion")!.getAttribute("data-motion");
  view.rerender(<Companion working={false} preview />);
  expect(motion()).toBe("idle");
  view.rerender(<Companion working={false} outcome="completed" preview />);
  expect(motion()).toBe("success");
  act(() => vi.advanceTimersByTime(3700));
  expect(motion()).toBe("idle");
  view.rerender(<Companion working preview />);
  view.rerender(<Companion working={false} outcome="cancelled" preview />);
  expect(motion()).toBe("idle");
});

it("shows waiting and failure using the actual task state", () => {
  const view = render(<Companion working waiting preview />);
  expect(view.container.querySelector(".companion")!.getAttribute("data-motion")).toBe("waiting");
  fireEvent.click(screen.getByRole("button"));
  expect(screen.getByRole("status").textContent).toContain("等你确认");
  view.rerender(<Companion working={false} outcome="failed" preview />);
  expect(view.container.querySelector(".companion")!.getAttribute("data-motion")).toBe("failed");
});

it("supports keyboard activation and clears pending replies on unmount", () => {
  const view = render(<Companion working={false} preview />);
  fireEvent.focus(screen.getByRole("button"));
  expect(view.container.querySelector(".companion")!.getAttribute("data-motion")).toBe("curious");
  fireEvent.click(screen.getByRole("button"));
  const first = screen.getByRole("status").textContent;
  fireEvent.click(screen.getByRole("button"));
  expect(screen.getByRole("status").textContent).not.toBe(first);
  view.unmount();
  expect(vi.getTimerCount()).toBe(0);
});
