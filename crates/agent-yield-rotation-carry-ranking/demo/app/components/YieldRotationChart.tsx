"use client";

import type { YieldRotationState, MarketMetrics } from "@/lib/yieldrotation-engine";

type Props = Readonly<{ yieldrotation: YieldRotationState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 20, PAD_B = 32;

// Palette for non-top bars.
const DIM = "#3f5b8a";
const TOP = "#f5a623"; // orange, "current top"

export function YieldRotationChart({ yieldrotation }: Props) {
  if (!yieldrotation || yieldrotation.ranking.length === 0) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No ranking yet — start Yield Rotation to see score bars per market.
      </div>
    );
  }
  const ranking = yieldrotation.ranking;
  const currentTop = yieldrotation.currentTop ?? ranking[0].market;

  const scores = ranking.map((r) => r.score);
  const maxScore = Math.max(...scores, 0);
  const minScore = Math.min(...scores, 0);
  const range = maxScore - minScore || 1;
  const yMax = maxScore + range * 0.15;
  const yMin = minScore - range * 0.1;
  const yRange = yMax - yMin || 1;

  const n = ranking.length;
  const chartW = W - PAD_L - PAD_R;
  const slot = chartW / n;
  const bw = Math.min(slot * 0.55, 80);

  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);
  const zeroY = y(0);

  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  return (
    <svg viewBox={`0 0 ${W + 20} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 20} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(2)}</text>
        </g>
      ))}
      {/* zero baseline */}
      <line x1={PAD_L} x2={W - PAD_R} y1={zeroY} y2={zeroY} stroke="#40404c" strokeWidth={1} />

      {ranking.map((r: MarketMetrics, i: number) => {
        const cx = PAD_L + slot * (i + 0.5);
        const top = r.score >= 0 ? y(r.score) : zeroY;
        const bot = r.score >= 0 ? zeroY : y(r.score);
        const h = Math.max(1, bot - top);
        const isTop = r.market === currentTop;
        const fill = isTop ? TOP : DIM;
        const opacity = isTop ? 0.95 : 0.55;
        return (
          <g key={r.market}>
            <rect x={cx - bw / 2} y={top} width={bw} height={h} fill={fill} opacity={opacity} stroke={fill} strokeWidth={1} />
            <text x={cx} y={H - PAD_B + 14} fill={isTop ? TOP : "var(--text-faint)"} fontSize={11} textAnchor="middle" fontFamily="ui-monospace, monospace" fontWeight={isTop ? 700 : 400}>{r.market}</text>
            <text x={cx} y={top - 6} fill={isTop ? TOP : "var(--text-faint)"} fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">{r.score.toFixed(2)}</text>
          </g>
        );
      })}
    </svg>
  );
}

type TimelineProps = Readonly<{ yieldrotation: YieldRotationState | null }>;

const TW = 720, TH = 120, T_PAD_L = 60, T_PAD_R = 20, T_PAD_T = 12, T_PAD_B = 20;

const LINE_COLORS = ["#4e9cff", "#5ce1a5", "#ff8c67", "#d072ff", "#ffd452", "#8dffec", "#ff8ac1", "#a4ff7f"];

export function ScoreTimeline({ yieldrotation }: TimelineProps) {
  if (!yieldrotation || yieldrotation.scoreHistory.length < 2) {
    return (
      <div style={{ height: TH, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)", fontSize: 12 }}>
        Awaiting more published cycles for the score timeline…
      </div>
    );
  }
  const history = yieldrotation.scoreHistory;
  const markets = yieldrotation.config.markets;
  const tMin = history[0].t, tMax = history[history.length - 1].t;
  const tRange = tMax - tMin || 1;
  let mn = Infinity, mx = -Infinity;
  for (const snap of history) for (const m of markets) {
    const v = snap.scores[m];
    if (typeof v === "number") { if (v < mn) mn = v; if (v > mx) mx = v; }
  }
  if (!Number.isFinite(mn) || !Number.isFinite(mx)) { mn = 0; mx = 1; }
  const pad = (mx - mn) * 0.1 || 1;
  const yMin = mn - pad, yMax = mx + pad, yRange = yMax - yMin || 1;
  const x = (t: number) => T_PAD_L + ((t - tMin) / tRange) * (TW - T_PAD_L - T_PAD_R);
  const y = (v: number) => T_PAD_T + (1 - (v - yMin) / yRange) * (TH - T_PAD_T - T_PAD_B);

  const paths = markets.map((m, mi) => {
    const d = history
      .map((snap, i) => {
        const v = snap.scores[m];
        if (typeof v !== "number") return null;
        return `${i === 0 ? "M" : "L"}${x(snap.t).toFixed(1)},${y(v).toFixed(1)}`;
      })
      .filter((s): s is string => s !== null)
      .join(" ");
    return { m, d, color: LINE_COLORS[mi % LINE_COLORS.length] };
  });

  return (
    <svg viewBox={`0 0 ${TW + 20} ${TH}`} style={{ width: "100%", height: "auto", maxHeight: 140 }}>
      <rect x={0} y={0} width={TW + 20} height={TH} fill="transparent" />
      {/* rotation markers */}
      {yieldrotation.rotationEvents
        .filter((e) => e.t >= tMin && e.t <= tMax)
        .map((e, i) => (
          <g key={`${e.t}-${i}`}>
            <line x1={x(e.t)} x2={x(e.t)} y1={T_PAD_T} y2={TH - T_PAD_B} stroke={TOP} strokeWidth={1} strokeDasharray="2,3" opacity={0.8} />
          </g>
        ))}
      {paths.map((p) => (
        <path key={p.m} d={p.d} stroke={p.color} strokeWidth={1.4} fill="none" opacity={0.85} />
      ))}
      {/* legend */}
      <g>
        {paths.map((p, i) => (
          <g key={`lg-${p.m}`} transform={`translate(${T_PAD_L + i * 110}, ${TH - 4})`}>
            <rect x={0} y={-8} width={10} height={2} fill={p.color} />
            <text x={14} y={-4} fill="var(--text-faint)" fontSize={9} fontFamily="ui-monospace, monospace">{p.m}</text>
          </g>
        ))}
      </g>
    </svg>
  );
}
