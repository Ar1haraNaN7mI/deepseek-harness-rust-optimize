import { useEffect, useRef } from "react";

declare global {
  interface Window {
    DSHEmblem?: { name: string; parts: number[][][][] };
  }
}
export function Emblem({
  size = 42,
  className = "",
}: {
  size?: number;
  className?: string;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const element = canvas.current;
    const ctx = element?.getContext("2d");
    if (!element || !ctx || !window.DSHEmblem) return;
    const ratio = Math.min(window.devicePixelRatio || 1, 3);
    element.width = size * ratio;
    element.height = size * ratio;
    ctx.scale(ratio, ratio);
    ctx.translate(size / 2, size / 2);
    ctx.scale(size / 212, size / 212);
    ctx.fillStyle = getComputedStyle(element).color;
    for (const part of window.DSHEmblem.parts) {
      ctx.beginPath();
      for (const contour of part) {
        contour.forEach(([x, y], index) =>
          index ? ctx.lineTo(x, y) : ctx.moveTo(x, y),
        );
        ctx.closePath();
      }
      ctx.fill("evenodd");
    }
  }, [size]);
  return (
    <canvas
      ref={canvas}
      className={className}
      style={{ width: size, height: size }}
      aria-label="DSH DELTA CIRCUIT 部门徽章"
      role="img"
    />
  );
}
