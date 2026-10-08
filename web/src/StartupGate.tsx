import { useEffect, useRef, useState, type ReactNode } from "react";

export type FinishReason = "complete" | "skip" | "unavailable";
type Props = {
  children: ReactNode;
  enabled?: boolean;
  startupUrl?: string;
  onFinish?: (reason: FinishReason) => void;
  onUnlock?: () => void;
  readyTimeoutMs?: number;
};
type Frame = { url: string; origin: string; channel: string };

/** Same-origin or explicit local-service iframe; no animation globals leak into React. */
export function StartupGate({
  children,
  enabled = false,
  startupUrl = "http://127.0.0.1:8769/startup-preview.html",
  onFinish,
  onUnlock,
  readyTimeoutMs = 12000,
}: Props) {
  const iframe = useRef<HTMLIFrameElement>(null);
  const callback = useRef(onFinish);
  callback.current = onFinish;
  const unlockCallback = useRef(onUnlock);
  unlockCallback.current = onUnlock;
  const finished = useRef(false);
  const [done, setDone] = useState(false);
  const [frame, setFrame] = useState<Frame>();
  const [unavailable, setUnavailable] = useState(false);
  const complete = (reason: FinishReason) => {
    if (finished.current) return;
    finished.current = true;
    setDone(true);
    callback.current?.(reason);
  };

  useEffect(() => {
    if (!enabled || finished.current) return;
    let alive = true;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      const url = new URL(startupUrl, window.location.href);
      if (!["http:", "https:"].includes(url.protocol))
        throw new Error("unsupported startup URL");
      const channel = crypto.randomUUID();
      url.searchParams.set("embed", "1");
      url.searchParams.set("parentOrigin", window.location.origin);
      url.searchParams.set("channel", channel);
      const next = { url: url.href, origin: url.origin, channel };
      setFrame(next);
      setUnavailable(false);
      const receive = (event: MessageEvent) => {
        const data = event.data;
        if (
          !alive ||
          event.origin !== next.origin ||
          event.source !== iframe.current?.contentWindow ||
          !data ||
          data.source !== "dsh-startup" ||
          data.channel !== channel
        )
          return;
        if (data.type === "ready") {
          clearTimeout(timeout);
          setUnavailable(false);
          iframe.current?.focus({ preventScroll: true });
        }
        if (data.type === "complete" || data.type === "skip")
          complete(data.type);
        if (data.type === "unlocked") unlockCallback.current?.();
      };
      window.addEventListener("message", receive);
      timeout = setTimeout(() => {
        if (alive) setUnavailable(true);
      }, readyTimeoutMs);
      return () => {
        alive = false;
        clearTimeout(timeout);
        window.removeEventListener("message", receive);
      };
    } catch {
      setUnavailable(true);
    }
    return () => {
      alive = false;
      clearTimeout(timeout);
    };
  }, [enabled, startupUrl, readyTimeoutMs]);

  if (!enabled || done) return <>{children}</>;
  return (
    <div className="startup-gate" aria-label="DSH 启动序列">
      {frame && (
        <iframe
          ref={iframe}
          title="DSH 沉浸式启动动画"
          src={frame.url}
          allow="autoplay"
          onError={() => setUnavailable(true)}
        />
      )}
      {unavailable && (
        <div className="startup-fallback" role="status">
          <p>启动动画暂时无法连接</p>
          <p>可以直接进入 Harness，稍后从设置中重播。</p>
          <button onClick={() => complete("unavailable")}>进入工作台</button>
        </div>
      )}
      {!unavailable && (
        <button className="gate-skip" onClick={() => complete("skip")}>
          跳过开场
        </button>
      )}
    </div>
  );
}
