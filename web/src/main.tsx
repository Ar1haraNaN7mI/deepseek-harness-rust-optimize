import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { StartupSurface } from "./StartupSurface";
import { bootstrap, isAbort } from "./api";
import { consumeInitialPlayback } from "./preferences";
import { consumeLaunchPlayback } from "./launchOverride";
import type { Bootstrap } from "./types";
import "./styles.css";
import "./ui-effects.css";
import "../../docs/startup-emblem.js";

function HarnessRoot() {
  const [boot, setBoot] = useState<{ data?: Bootstrap }>();
  const [play, setPlay] = useState(false);
  const [generation, setGeneration] = useState(0);
  useEffect(() => {
    const abort = new AbortController();
    bootstrap(abort.signal)
      .then((data) => {
        if (abort.signal.aborted) return;
        setPlay(
          consumeLaunchPlayback(() =>
            consumeInitialPlayback(
              data.startup.enabled,
              data.startup.override_enabled,
            ),
          ),
        );
        setBoot({ data });
      })
      .catch((error) => {
        if (!isAbort(error)) setBoot({});
      });
    return () => abort.abort();
  }, []);
  if (!boot)
    return (
      <div className="initial-connection" role="status">
        <span className="eyebrow">DSH / LOCAL WORKSPACE</span>
        <p>正在连接本机 Harness…</p>
      </div>
    );
  return (
    <StartupSurface
      playing={play}
      generation={generation}
      onFinish={() => setPlay(false)}
    >
      <App
        initialData={boot.data}
        onReplay={() => {
          setGeneration((value) => value + 1);
          setPlay(true);
        }}
      />
    </StartupSurface>
  );
}
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <HarnessRoot />
  </StrictMode>,
);
