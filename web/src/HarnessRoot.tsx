import { useCallback, useEffect, useRef, useState } from "react";
import { App } from "./App";
import { StartupSurface } from "./StartupSurface";
import { AccessEntry } from "./AccessEntry";
import { accessStatus, bootstrap, errorText, isAbort } from "./api";
import { consumeInitialPlayback } from "./preferences";
import { consumeLaunchPlayback } from "./launchOverride";
import { useInterfaceEffects } from "./uiPreferences";
import type { AccessStatus, Bootstrap } from "./types";
import "./access.css";

/** Protected data is never requested or mounted until the service grants access. */
export function HarnessRoot() {
  useInterfaceEffects();
  const [access, setAccess] = useState<AccessStatus>();
  const [boot, setBoot] = useState<Bootstrap>();
  const [play, setPlay] = useState(false);
  const [generation, setGeneration] = useState(0);
  const [error, setError] = useState("");
  const [checking, setChecking] = useState(false);
  const active = useRef<AbortController | undefined>(undefined);
  const playbackChosen = useRef(false);

  useEffect(() => {
    if (!access || access.unlocked) return;
    const desktop = window as typeof window & {
      __DSH_DESKTOP__?: boolean;
      ipc?: { postMessage?: (message: string) => void };
    };
    // A verified locked entry is a usable frontend too. Do not fetch private
    // bootstrap data merely to satisfy the native window's readiness check.
    if (desktop.__DSH_DESKTOP__ && typeof desktop.ipc?.postMessage === "function") {
      desktop.ipc.postMessage(JSON.stringify({ event: "dsh-ready", version: 1 }));
    }
  }, [access]);

  const connect = useCallback(async (choosePlayback = false) => {
    active.current?.abort();
    const abort = new AbortController();
    active.current = abort;
    setChecking(true);
    setError("");
    try {
      const status = await accessStatus(abort.signal);
      if (abort.signal.aborted) return;
      setAccess(status);
      if (choosePlayback && !playbackChosen.current) {
        playbackChosen.current = true;
        setPlay(consumeLaunchPlayback(() => consumeInitialPlayback(
          status.startup.enabled, status.startup.override_enabled,
        )));
      }
      if (!status.unlocked) {
        setBoot(undefined);
        return;
      }
      const data = await bootstrap(abort.signal);
      if (!abort.signal.aborted) setBoot(data);
    } catch (failure) {
      if (!abort.signal.aborted && !isAbort(failure)) {
        setError(errorText(failure));
        // A failed recheck must be visible, including while the film owns focus.
        setPlay(false);
      }
    } finally {
      if (!abort.signal.aborted) setChecking(false);
    }
  }, []);

  useEffect(() => {
    void connect(true);
    const locked = () => {
      // A revoked session must also remove conversations already rendered.
      setBoot(undefined);
      setPlay(false);
      void connect();
    };
    window.addEventListener("dsh-access-locked", locked);
    return () => {
      active.current?.abort();
      window.removeEventListener("dsh-access-locked", locked);
    };
  }, [connect]);

  return (
    <StartupSurface
      playing={play}
      generation={generation}
      onUnlock={() => void connect()}
      onFinish={() => {
        setPlay(false);
        if (!boot) void connect();
      }}
    >
      {access && !access.unlocked ? (
        <AccessEntry status={access} onUnlocked={() => connect()} connectionError={error} checking={checking} onRetry={() => connect()} />
      ) : boot ? (
        <App initialData={boot} onReplay={() => {
          setGeneration((value) => value + 1);
          setPlay(true);
        }} />
      ) : (
        <div className="initial-connection" role={error ? "alert" : "status"}>
          <span className="eyebrow">DSH / LOCAL WORKSPACE</span>
          <p>{error || "正在连接本机 Harness…"}</p>
          {error && <button className="outline-button" disabled={checking} onClick={() => void connect(true)}>重新连接</button>}
        </div>
      )}
    </StartupSurface>
  );
}
