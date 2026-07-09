"use client";

import type { Tick } from "@/lib/store";
import type { TrendAnalysisState } from "@/lib/trendanalysis-engine";

type Props = Readonly<{ ticks: readonly Tick[]; trendanalysis: TrendAnalysisState | null }>;

const W = 720;
const PAD_L = 60, PAD_R = 90, PAD_T = 14, PAD_B = 24;
const H = 340;

export function TrendAnalysisChart({ ticks, trendanalysis }: Props) {
  if (ticks.length === 0 || !trendanalysis) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Multi-Timeframe to see mid and the three SMAs.</div>;
  }

  const prices = ticks.map((t) => t.price);
  const shortSmas = ticks.map((t) => t.smaShort).filter((v): v is number => v !== null && Number.isFinite(v));
  const midSmas = ticks.map((t) => t.smaMid).filter((v): v is number => v !== null && Number.isFinite(v));
  const longSmas = ticks.map((t) => t.smaLong).filter((v): v is number => v !== null && Number.isFinite(v));

  const minAll = Math.min(...prices, ...shortSmas, ...midSmas, ...longSmas);
  const maxAll = Math.max(...prices, ...shortSmas, ...midSmas, ...longSmas);
  const pad = (maxAll - minAll) * 0.1 || Math.abs(minAll) * 0.02 || 0.01;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;

  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;

  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pathFrom = <T,>(sel: (t: Tick) => T | null) => {
    let started = false;
    const cmds: string[] = [];
    for (const t of ticks) {
      const v = sel(t);
      if (v === null || typeof v !== "number" || !Number.isFinite(v)) continue;
      const px = x(t.t).toFixed(1);
      const py = y(v as number).toFixed(1);
      cmds.push(`${started ? "L" : "M"}${px},${py}`);
      started = true;
    }
    return cmds.join(" ");
  };

  const pricePath = pathFrom((t) => t.price);
  const shortPath = pathFrom((t) => t.smaShort);
  const midPath = pathFrom((t) => t.smaMid);
  const longPath = pathFrom((t) => t.smaLong);

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const last = ticks[ticks.length - 1];

  const alignColor = (() => {
    switch (last.alignment) {
      case "aligned_up":   return "#6de6a3";
      case "aligned_down": return "#ff8dab";
      case "mixed":        return "#ffd166";
      case "warmup":
      default:             return "#7b8296";
    }
  })();

  const c = trendanalysis.config;

  return (
    <svg viewBox={`0 0 ${W + 10} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 460 }}>
      <rect x={0} y={0} width={W + 10} height={H} fill="transparent" />

      {yTicks.map((v, i) => (
        <g key={`g-${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Long SMA (slowest) */}
      {longPath && <path d={longPath} stroke="#3f5b8a" strokeWidth={1.4} fill="none" opacity={0.9} />}
      {/* Mid SMA */}
      {midPath && <path d={midPath} stroke="#ffd166" strokeWidth={1.4} fill="none" opacity={0.9} />}
      {/* Short SMA */}
      {shortPath && <path d={shortPath} stroke="var(--accent)" strokeWidth={1.6} fill="none" opacity={0.95} />}
      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      <circle cx={x(tMax)} cy={y(last.price)} r={3.5} fill="#ececf1" />

      <text x={W - PAD_R + 6} y={y(last.price) + 3} fill="#ececf1" fontSize={10} fontFamily="ui-monospace, monospace">MID {last.price.toFixed(digits)}</text>
      {last.smaShort !== null && Number.isFinite(last.smaShort) && (
        <text x={W - PAD_R + 6} y={y(last.smaShort) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">S({c.short}) {last.smaShort.toFixed(digits)}</text>
      )}
      {last.smaMid !== null && Number.isFinite(last.smaMid) && (
        <text x={W - PAD_R + 6} y={y(last.smaMid) + 3} fill="#ffd166" fontSize={10} fontFamily="ui-monospace, monospace">M({c.mid}) {last.smaMid.toFixed(digits)}</text>
      )}
      {last.smaLong !== null && Number.isFinite(last.smaLong) && (
        <text x={W - PAD_R + 6} y={y(last.smaLong) + 3} fill="#3f5b8a" fontSize={10} fontFamily="ui-monospace, monospace">L({c.long}) {last.smaLong.toFixed(digits)}</text>
      )}

      <text x={PAD_L} y={H - 6} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
        mid · SMA short({c.short}) · SMA mid({c.mid}) · SMA long({c.long})
      </text>
      <text x={W - PAD_R} y={H - 6} fill={alignColor} fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">
        alignment: {last.alignment}
      </text>
    </svg>
  );
}
