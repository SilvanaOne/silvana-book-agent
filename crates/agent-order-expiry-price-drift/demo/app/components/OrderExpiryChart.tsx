"use client";

import type { Tick } from "@/lib/store";
import type { OrderExpiryState, OrderExpiryOrder } from "@/lib/orderexpiry-engine";

type Props = Readonly<{ ticks: readonly Tick[]; orderexpiry: OrderExpiryState | null }>;

const W = 720, H = 320, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

// Price chart: the mid-price walk as a line, with each own-order drawn
// as a small horizontal marker at its resting price (BID = green,
// OFFER = red). A dashed band around the current mid shows the
// max-drift envelope: any marker outside the band is a cancel candidate.
// Cancelled orders end at a red × where the sweep pulled them.
export function OrderExpiryChart({ ticks, orderexpiry }: Props) {
  if (ticks.length === 0 || !orderexpiry) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Order Expiry to see the price / drift chart.</div>;
  }

  const now = ticks[ticks.length - 1].t;
  const tMin = ticks[0].t;
  const tMax = now;
  const tRange = tMax - tMin || 1;

  const orders = orderexpiry.orders.slice(-80);

  // Y-domain: cover mid range and all order prices.
  const prices: number[] = ticks.map((p) => p.price);
  for (const o of orders) prices.push(o.price);
  const currentMid = orderexpiry.currentPrice;
  const drift = orderexpiry.config.maxDriftPct / 100;
  if (currentMid > 0 && drift > 0) {
    prices.push(currentMid * (1 + drift));
    prices.push(currentMid * (1 - drift));
  }
  const pMin = Math.min(...prices);
  const pMax = Math.max(...prices);
  const pRange = (pMax - pMin) || 1;
  const pad = pRange * 0.06;
  const yMin = pMin - pad;
  const yMax = pMax + pad;

  const x = (t: number) => PAD_L + ((Math.max(t, tMin) - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (p: number) => {
    const t = (p - yMin) / (yMax - yMin);
    return H - PAD_B - t * (H - PAD_T - PAD_B);
  };

  // X-axis: labelled time offsets.
  const xTicks: number[] = [];
  for (let i = 0; i <= 4; i++) xTicks.push(tMin + (tRange * i) / 4);

  // Y-axis ticks: 4 evenly spaced price labels.
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + ((yMax - yMin) * i) / 4);

  // Mid-price polyline.
  const midPath = ticks
    .map((tk, i) => `${i === 0 ? "M" : "L"} ${x(tk.t).toFixed(1)} ${y(tk.price).toFixed(1)}`)
    .join(" ");

  // Max-drift envelope around current mid (drawn as a horizontal band on the right).
  const upperY = currentMid > 0 ? y(currentMid * (1 + drift)) : null;
  const lowerY = currentMid > 0 ? y(currentMid * (1 - drift)) : null;
  const midY = currentMid > 0 ? y(currentMid) : null;

  return (
    <svg viewBox={`0 0 ${W + 20} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W + 20} height={H} fill="transparent" />

      {/* X grid + labels */}
      {xTicks.map((t, i) => (
        <g key={`xt${i}`}>
          <line x1={x(t)} x2={x(t)} y1={PAD_T} y2={H - PAD_B} stroke="#1c2647" strokeWidth={1} />
          <text x={x(t)} y={H - PAD_B + 14} fill="var(--text-faint)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">{Math.round((t - tMin) / 1000)}s</text>
        </g>
      ))}

      {/* Y grid + labels */}
      {yTicks.map((p, i) => (
        <g key={`yt${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(p)} y2={y(p)} stroke="#1c2647" strokeWidth={1} opacity={0.5} />
          <text x={PAD_L - 6} y={y(p) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{fmtPrice(p)}</text>
        </g>
      ))}

      {/* Drift envelope (current mid ± maxDriftPct) */}
      {upperY !== null && lowerY !== null && (
        <>
          <rect x={PAD_L} y={upperY} width={W - PAD_L - PAD_R} height={Math.max(0, lowerY - upperY)} fill="var(--accent)" opacity={0.06} />
          <line x1={PAD_L} x2={W - PAD_R} y1={upperY} y2={upperY} stroke="var(--accent)" strokeDasharray="4,3" strokeWidth={1} opacity={0.6} />
          <line x1={PAD_L} x2={W - PAD_R} y1={lowerY} y2={lowerY} stroke="var(--accent)" strokeDasharray="4,3" strokeWidth={1} opacity={0.6} />
        </>
      )}

      {/* Mid-price line */}
      <path d={midPath} fill="none" stroke="var(--accent)" strokeWidth={1.5} opacity={0.9} />

      {/* Current mid marker on the right edge */}
      {midY !== null && (
        <>
          <circle cx={x(now)} cy={midY} r={3.5} fill="var(--accent)" />
          <text x={x(now) + 6} y={midY - 4} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">mid {fmtPrice(currentMid)}</text>
        </>
      )}

      {/* Order life segments — line from createdAt to end at its resting price */}
      {orders.map((o: OrderExpiryOrder) => {
        const start = o.createdAt;
        const end = o.status === "cancelled" ? (o.cancelledAt ?? now) : now;
        const sx = x(start);
        const ex = x(end);
        const oy = y(o.price);
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";

        return (
          <g key={o.seq}>
            <line x1={sx} x2={ex} y1={oy} y2={oy} stroke={color} strokeWidth={1.4} opacity={o.status === "cancelled" ? 0.5 : 0.85} />
            <circle cx={sx} cy={oy} r={2} fill={color} />
            {o.status === "cancelled" && (
              <g stroke="#ff6b7a" strokeWidth={1.6}>
                <line x1={ex - 4} x2={ex + 4} y1={oy - 4} y2={oy + 4} />
                <line x1={ex - 4} x2={ex + 4} y1={oy + 4} y2={oy - 4} />
              </g>
            )}
          </g>
        );
      })}

      {/* Legend */}
      <g transform={`translate(${PAD_L}, ${H - 6})`}>
        <rect x={0} y={-7} width={10} height={6} fill="var(--pos)" opacity={0.7} />
        <text x={14} y={-2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">BID</text>
        <rect x={50} y={-7} width={10} height={6} fill="var(--neg)" opacity={0.7} />
        <text x={64} y={-2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">OFFER</text>
        <g transform="translate(110, -4)" stroke="#ff6b7a" strokeWidth={1.6}>
          <line x1={-4} x2={4} y1={-4} y2={4} />
          <line x1={-4} x2={4} y1={4} y2={-4} />
        </g>
        <text x={120} y={-2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">cancelled</text>
        <text x={200} y={-2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">dashed band = ±max-drift envelope around mid</text>
      </g>
    </svg>
  );
}

function fmtPrice(n: number): string {
  const abs = Math.abs(n);
  if (abs > 100) return n.toFixed(2);
  if (abs > 1) return n.toFixed(4);
  return n.toFixed(6);
}
