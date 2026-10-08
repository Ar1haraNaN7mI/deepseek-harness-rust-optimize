import type { ReactNode } from "react";
import { StartupGate, type FinishReason } from "./StartupGate";
import { useUiPreferences } from "./uiPreferences";

/** Keep the working application mounted while the isolated film owns focus. */
export function StartupSurface({
  playing,
  generation,
  onFinish,
  onUnlock,
  children,
}: {
  playing: boolean;
  generation: number;
  onFinish: (reason: FinishReason) => void;
  onUnlock?: () => void;
  children: ReactNode;
}) {
  const [preferences] = useUiPreferences();
  return (
    <>
      <div hidden={playing} inert={playing}>
        {children}
      </div>
      <StartupGate
        key={generation}
        enabled={playing}
        startupUrl={preferences.reducedMotion === "reduce" ? "/startup-preview.html?motion=reduce" : "/startup-preview.html"}
        onFinish={onFinish}
        onUnlock={onUnlock}
      >
        {null}
      </StartupGate>
    </>
  );
}
