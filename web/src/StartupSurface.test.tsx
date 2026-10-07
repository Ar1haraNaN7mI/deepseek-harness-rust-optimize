import { useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { StartupSurface } from "./StartupSurface";

afterEach(cleanup);
function Work() {
  const [draft, setDraft] = useState("");
  return (
    <input
      aria-label="draft"
      value={draft}
      onChange={(event) => setDraft(event.target.value)}
    />
  );
}
it("preserves the mounted working app and its draft throughout replay", () => {
  const finish = vi.fn();
  const view = render(
    <StartupSurface playing={false} generation={0} onFinish={finish}>
      <Work />
    </StartupSurface>,
  );
  fireEvent.change(screen.getByLabelText("draft"), {
    target: { value: "keep this task" },
  });
  view.rerender(
    <StartupSurface playing generation={1} onFinish={finish}>
      <Work />
    </StartupSurface>,
  );
  expect(screen.getByTitle("DSH 沉浸式启动动画")).toBeTruthy();
  expect((screen.getByLabelText("draft") as HTMLInputElement).value).toBe(
    "keep this task",
  );
  fireEvent.click(screen.getByText("跳过开场"));
  view.rerender(
    <StartupSurface playing={false} generation={1} onFinish={finish}>
      <Work />
    </StartupSurface>,
  );
  expect((screen.getByLabelText("draft") as HTMLInputElement).value).toBe(
    "keep this task",
  );
  expect(screen.queryByTitle("DSH 沉浸式启动动画")).toBeNull();
});
