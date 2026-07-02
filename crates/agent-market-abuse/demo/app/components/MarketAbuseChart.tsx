"use client";

import type { Tick } from "@/lib/store";
import type { MarketAbuseState } from "@/lib/marketabuse-engine";

type Props = Readonly<{ ticks: readonly Tick[]; marketabuse: MarketAbuseState | null }>;

const W = 720;
const H_TOP = 220;
const H_BOT = 90;
const H = H_TOP + H_BOT + 30;
const PAD_L = 60;
const PAD_R = 20;
const PAD_T = 14;
const PAD_B = 26;

export function MarketAbuseChart({ ticks, marketabuse }: Props) {
  if (ticks.length === 0 || !marketabuse) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Market Abuse to see mid, orders, cancels and alerts.</div>;
  }

  const prices = ticks.map((t) => t.price);
  const minAll = Math.min(...prices);
  const maxAll = Math.max(...prices);
  const pad = (maxAll - minAll) * 0.15 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = ticks[0].t;
  const tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const yTop = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H_TOP - PAD_T - PAD_B);

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yTop(t.price).toFixed(1)}`).join(" ");

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);
  const lastPrice = ticks[ticks.length - 1].price;

  // Filter orders whose createdAt is within the visible x range.
  const visibleOrders = marketabuse.allOrders.filter((o) => o.createdAt >= tMin && o.createdAt <= tMax);
  const visibleAlerts = marketabuse.alerts.filter((a) => a.t >= tMin && a.t <= tMax);

  // Bottom panel: alert counts per kind.
  const spoofCount = marketabuse.spoofsDetected;
  const layerCount = marketabuse.layersDetected;
  const maxBar = Math.max(1, spoofCount, layerCount);
  const barY = H_TOP + 30;
  const barH = H_BOT - 20;
  const barLeftSpoof = PAD_L + 20;
  const barLeftLayer = PAD_L + 180;
  const barWidth = 80;
  const barVal = (v: number) => (v / maxBar) * barH;

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />

      {/* Top panel: mid + order dots + cancel crosses + alert markers */}
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yTop(v)} y2={yTop(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yTop(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Alert vertical markers (behind everything) */}
      {visibleAlerts.map((a, i) => {
        const color = a.kind === "spoof" ? "#f6c94f" : "#f6884f";
        return (
          <line key={`al-${i}`} x1={x(a.t)} x2={x(a.t)} y1={PAD_T} y2={H_TOP - PAD_B} stroke={color} strokeWidth={1.2} strokeDasharray="2,3" opacity={0.7} />
        );
      })}

      {/* Order creation dots */}
      {visibleOrders.map((o) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        return <circle key={`o-${o.seq}`} cx={x(o.createdAt)} cy={yTop(o.price)} r={2} fill={color} opacity={0.75} />;
      })}

      {/* Cancel crosses */}
      {visibleOrders.filter((o) => o.status === "cancelled" && o.cancelledAt !== undefined).map((o) => {
        const cx0 = x(o.cancelledAt as number);
        const cy0 = yTop(o.price);
        const r = 3;
        return (
          <g key={`c-${o.seq}`}>
            <line x1={cx0 - r} y1={cy0 - r} x2={cx0 + r} y2={cy0 + r} stroke="#ff5a5a" strokeWidth={1.2} />
            <line x1={cx0 - r} y1={cy0 + r} x2={cx0 + r} y2={cy0 - r} stroke="#ff5a5a" strokeWidth={1.2} />
          </g>
        );
      })}

      {/* Mid price on top */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />
      <circle cx={x(tMax)} cy={yTop(lastPrice)} r={3.5} fill="#ececf1" />
      <text x={W - PAD_R + 4} y={yTop(lastPrice) + 3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">mid {lastPrice.toFixed(digits)}</text>

      {/* Divider */}
      <line x1={PAD_L} x2={W - PAD_R} y1={H_TOP + 4} y2={H_TOP + 4} stroke="#22222c" strokeWidth={1} />
      <text x={PAD_L} y={H_TOP + 20} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">ALERTS BY KIND</text>

      {/* Bottom panel: alert bars */}
      <g>
        <rect x={barLeftSpoof} y={barY + (barH - barVal(spoofCount))} width={barWidth} height={barVal(spoofCount)} fill="#f6c94f" opacity={0.85} />
        <text x={barLeftSpoof + barWidth / 2} y={barY + barH + 14} fill="var(--text-faint)" fontSize={11} textAnchor="middle">spoof: {spoofCount}</text>

        <rect x={barLeftLayer} y={barY + (barH - barVal(layerCount))} width={barWidth} height={barVal(layerCount)} fill="#f6884f" opacity={0.85} />
        <text x={barLeftLayer + barWidth / 2} y={barY + barH + 14} fill="var(--text-faint)" fontSize={11} textAnchor="middle">layer: {layerCount}</text>

        <text x={barLeftLayer + barWidth + 40} y={barY + 12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">active orders: {marketabuse.activeOrders.length}</text>
        <text x={barLeftLayer + barWidth + 40} y={barY + 28} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">cancelled: {marketabuse.ordersCancelled}</text>
        <text x={barLeftLayer + barWidth + 40} y={barY + 44} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">created: {marketabuse.ordersCreated}</text>
      </g>
    </svg>
  );
}
