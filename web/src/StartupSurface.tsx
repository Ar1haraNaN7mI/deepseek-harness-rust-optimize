import type { ReactNode } from "react";
import { StartupGate, type FinishReason } from "./StartupGate";

/** Keep the working application mounted while the isolated film owns focus. */
export function StartupSurface({
  playing,
  generation,
  onFinish,
  children,
}: {
  playing: boolean;
  generation: number;
  onFinish: (reason: FinishReason) => void;
  children: ReactNode;
}) {
  return (
    <>
      <div hidden={playing} inert={playing}>
        {children}
      </div>
      <StartupGate
        key={generation}
        enabled={playing}
        startupUrl="/startup-preview.html"
        onFinish={onFinish}
      >
        {null}
      </StartupGate>
    </>
  );
}
