import { useEffect, useRef, useState, type CSSProperties } from "react";

// Exact upstream V2 atlas: 8 columns, 11 rows, 192 × 208 cells. The first
// nine rows contain different numbers of frames; unused cells are transparent.
const motions = {
  idle: { row: 0, frames: 7, seconds: 2.8 },
  waving: { row: 3, frames: 4, seconds: 1.4 },
  curious: { row: 4, frames: 5, seconds: 1.8 },
  failed: { row: 5, frames: 8, seconds: 2.4 },
  waiting: { row: 6, frames: 6, seconds: 3.2 },
  working: { row: 7, frames: 6, seconds: 1.8 },
  success: { row: 8, frames: 6, seconds: 1.8 },
} as const;
type Motion = keyof typeof motions;
type Props = { working: boolean; waiting?: boolean; outcome?: string | null; preview?: boolean };

export function Companion({ working, waiting = false, outcome, preview = false }: Props) {
  const [greeting, setGreeting] = useState(!preview);
  const [hovered, setHovered] = useState(false);
  const [response, setResponse] = useState("");
  const [celebration, setCelebration] = useState<"success" | "failed" | null>(null);
  const replyIndex = useRef(0);
  const awaitingOutcome = useRef(false);
  useEffect(() => {
    if (!greeting) return;
    const timer = window.setTimeout(() => setGreeting(false), 2100);
    return () => window.clearTimeout(timer);
  }, [greeting]);
  useEffect(() => {
    if (!response) return;
    const timer = window.setTimeout(() => setResponse(""), 3500);
    return () => window.clearTimeout(timer);
  }, [response]);
  useEffect(() => {
    if (working) { awaitingOutcome.current = true; setCelebration(null); return; }
    // A completed event and its durable status can arrive separately. Wait
    // for the real outcome, rather than celebrating any stop or cancellation.
    if (!awaitingOutcome.current || !outcome || outcome === "running") return;
    awaitingOutcome.current = false;
    if (outcome === "completed") setCelebration("success");
    else if (outcome === "failed") setCelebration("failed");
  }, [working, outcome]);
  useEffect(() => {
    if (!celebration) return;
    const timer = window.setTimeout(() => setCelebration(null), 3600);
    return () => window.clearTimeout(timer);
  }, [celebration]);

  const motion: Motion = waiting ? "waiting" : working ? "working" : celebration || (response || greeting ? "waving" : hovered ? "curious" : "idle");
  const { row, frames, seconds } = motions[motion];
  const status = waiting ? "等你确认操作" : working ? "陪你认真工作" : celebration === "success" ? "这项任务完成啦" : celebration === "failed" ? "一起看看哪里出错了" : "点一下，打个招呼";
  const greet = () => {
    const replies = waiting
      ? ["有一项操作在等你确认。", "先看看授权请求，我在这里陪你。"]
      : working
        ? ["收到，我陪你等这次结果。", "正在认真记笔记，请稍等一下。", "加油，今天也一起完成。"]
        : ["大肥鱼已就位，今天也请多关照。", "摸摸头收到啦。", "休息一下，再一起出发吧。"];
    setResponse(replies[replyIndex.current++ % replies.length]);
  };
  return <div className={`companion${preview ? " companion-preview" : ""}`} data-motion={motion}>
    <button
      className="companion-pet"
      type="button"
      aria-label={`与 DeepSeek 大肥鱼打招呼，${status}`}
      onClick={greet}
      onPointerEnter={() => setHovered(true)}
      onPointerLeave={() => setHovered(false)}
      onFocus={() => setHovered(true)}
      onBlur={() => setHovered(false)}
    >
      <span className="companion-sprite" aria-hidden="true" style={{
        "--pet-row": row, "--pet-frames": frames, "--pet-duration": `${seconds}s`,
      } as CSSProperties} />
    </button>
    <div className="companion-caption"><b>DeepSeek 大肥鱼</b><span>{status}</span></div>
    {response && <p className="companion-reply" role="status">{response}</p>}
  </div>;
}
