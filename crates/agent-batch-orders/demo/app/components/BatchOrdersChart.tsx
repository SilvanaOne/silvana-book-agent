"use client";

import type { Tick } from "@/lib/store";
import type { BatchOrdersState, BatchOrder } from "@/lib/batchorders-engine";
import { meanPrice } from "@/lib/batchorders-engine";

type Props = Readonly<{ ticks: readonly Tick[]; batchorders: BatchOrdersState | null }>;

const W = 720, H = 260, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

const STATUS_COLOR: Record<BatchOrder["status"], string> = {
  pending: "#6b7280",
  submitted: "#3f5b8a",
  filled: "#22c55e",
  failed: "#ef4444",
  cancelled: "#a1a1aa",
};

function StatBar({
  label,
  value,
  total,
  color,
}: Readonly<{ label: string; value: number; total: number; color: string }>) {
  const pct = total > 0 ? (value / total) * 100 : 0;
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12 }}>
      <div style={{ width: 78, color: "var(--text-faint)", textTransform: "uppercase", letterSpacing: 0.5 }}>{label}</div>
      <div style={{ flex: 1, background: "#1c1c24", borderRadius: 4, height: 14, overflow: "hidden", position: "relative" }}>
        <div style={{ width: `${pct}%`, background: color, height: "100%", transition: "width 300ms ease" }} />
      </div>
      <div className="mono" style={{ width: 60, textAlign: "right" }}>{value} / {total}</div>
    </div>
  );
}

export function BatchOrdersChart({ ticks, batchorders }: Props) {
  if (!batchorders) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — submit a batch to see progress, mid price and per-order status.
      </div>
    );
  }

  const total = batchorders.batch.length;
  const s = batchorders.stats;
  const meanBid = meanPrice(batchorders.batch, "BID");
  const meanOffer = meanPrice(batchorders.batch, "OFFER");

  // Sparkline
  let svg: React.ReactElement | null = null;
  if (ticks.length > 0) {
    const prices = ticks.map((t) => t.price);
    const orderPrices = batchorders.batch.map((o) => o.price);
    const refPrices: number[] = [];
    if (meanBid !== null) refPrices.push(meanBid);
    if (meanOffer !== null) refPrices.push(meanOffer);
    const minAll = Math.min(...prices, ...orderPrices, ...refPrices);
    const maxAll = Math.max(...prices, ...orderPrices, ...refPrices);
    const pad = (maxAll - minAll) * 0.1 || minAll * 0.02;
    const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
    const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
    const tRange = tMax - tMin || 1;
    const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
    const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);
    const pricePath = ticks
      .map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`)
      .join(" ");
    const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
    const yTicks: number[] = [];
    for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);
    const lastPrice = ticks[ticks.length - 1].price;

    svg = (
      <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 300 }}>
        <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
        {yTicks.map((v, i) => (
          <g key={i}>
            <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
            <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">
              {v.toFixed(digits)}
            </text>
          </g>
        ))}

        {/* mean(bids) / mean(offers) horizontals */}
        {meanBid !== null && (
          <g>
            <line
              x1={PAD_L}
              x2={W - PAD_R}
              y1={y(meanBid)}
              y2={y(meanBid)}
              stroke="var(--pos)"
              strokeWidth={1}
              strokeDasharray="4,4"
              opacity={0.6}
            />
            <text x={W - PAD_R + 4} y={y(meanBid) + 3} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace">
              μBID {meanBid.toFixed(digits)}
            </text>
          </g>
        )}
        {meanOffer !== null && (
          <g>
            <line
              x1={PAD_L}
              x2={W - PAD_R}
              y1={y(meanOffer)}
              y2={y(meanOffer)}
              stroke="var(--neg)"
              strokeWidth={1}
              strokeDasharray="4,4"
              opacity={0.6}
            />
            <text x={W - PAD_R + 4} y={y(meanOffer) + 3} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace">
              μOFR {meanOffer.toFixed(digits)}
            </text>
          </g>
        )}

        {/* Mid */}
        <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

        {/* Every order — colored by status; shape by side */}
        {batchorders.batch.map((o) => {
          const color = STATUS_COLOR[o.status];
          const t = o.submittedAt ?? o.t ?? tMin;
          const cx = x(Math.max(tMin, Math.min(tMax, t)));
          const cy = y(o.price);
          if (o.side === "BID") {
            return <circle key={o.seq} cx={cx} cy={cy} r={4.5} fill={o.status === "filled" ? color : "var(--bg-card)"} stroke={color} strokeWidth={1.6} />;
          }
          const size = 5.5;
          return (
            <rect
              key={o.seq}
              x={cx - size / 2}
              y={cy - size / 2}
              width={size}
              height={size}
              fill={o.status === "filled" ? color : "var(--bg-card)"}
              stroke={color}
              strokeWidth={1.6}
              transform={`rotate(45 ${cx} ${cy})`}
            />
          );
        })}

        <circle cx={x(tMax)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />
      </svg>
    );
  }

  return (
    <div className="stack" style={{ gap: 10 }}>
      <div className="stack" style={{ gap: 6 }}>
        <StatBar label="pending" value={s.pending} total={total} color={STATUS_COLOR.pending} />
        <StatBar label="submitted" value={s.submitted} total={total} color={STATUS_COLOR.submitted} />
        <StatBar label="filled" value={s.filled} total={total} color={STATUS_COLOR.filled} />
        <StatBar label="failed" value={s.failed} total={total} color={STATUS_COLOR.failed} />
        <StatBar label="cancelled" value={s.cancelled} total={total} color={STATUS_COLOR.cancelled} />
      </div>
      {svg}
    </div>
  );
}
