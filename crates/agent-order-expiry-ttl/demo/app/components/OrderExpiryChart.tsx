"use client";

import type { Tick } from "@/lib/store";
import type { OrderExpiryState, OrderExpiryOrder } from "@/lib/orderexpiry-engine";

type Props = Readonly<{ ticks: readonly Tick[]; orderexpiry: OrderExpiryState | null }>;

const W = 720, H = 320, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

// Gantt-style timeline. Each order is a horizontal "life bar" from
// createdAt → (cancelledAt or now). BID=green, OFFER=red. A red ×
// marks the moment of cancel. Order-index (creation order) on Y.
export function OrderExpiryChart({ ticks, orderexpiry }: Props) {
  if (ticks.length === 0 || !orderexpiry) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Order Expiry to see the order-lifetime timeline.</div>;
  }

  const now = ticks[ticks.length - 1].t;
  const tMin = ticks[0].t;
  const tMax = now;
  const tRange = tMax - tMin || 1;

  // Show up to the last N orders to keep the chart legible.
  const MAX_ROWS = 22;
  const allOrders = orderexpiry.orders;
  const orders = allOrders.slice(-MAX_ROWS);
  const rowCount = Math.max(orders.length, 1);

  const x = (t: number) => PAD_L + ((Math.max(t, tMin) - tMin) / tRange) * (W - PAD_L - PAD_R);
  const rowHeight = (H - PAD_T - PAD_B) / Math.max(rowCount, 6);
  const y = (i: number) => PAD_T + (i + 0.5) * rowHeight;

  const ttlMs = orderexpiry.config.maxAgeSecs * 1000;

  // X-axis: labelled time offsets (s since start of window).
  const xTicks: number[] = [];
  for (let i = 0; i <= 4; i++) xTicks.push(tMin + (tRange * i) / 4);

  return (
    <svg viewBox={`0 0 ${W + 20} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W + 20} height={H} fill="transparent" />

      {/* Grid + X-axis tick labels (seconds since window start) */}
      {xTicks.map((t, i) => (
        <g key={i}>
          <line x1={x(t)} x2={x(t)} y1={PAD_T} y2={H - PAD_B} stroke="#1c2647" strokeWidth={1} />
          <text x={x(t)} y={H - PAD_B + 14} fill="var(--text-faint)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">{Math.round((t - tMin) / 1000)}s</text>
        </g>
      ))}

      {/* "Now" vertical marker */}
      <line x1={x(now)} x2={x(now)} y1={PAD_T} y2={H - PAD_B} stroke="var(--accent)" strokeWidth={1} strokeDasharray="2,3" opacity={0.6} />
      <text x={x(now) + 4} y={PAD_T + 10} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">now</text>

      {/* One row per order — a life-bar from createdAt to end. */}
      {orders.map((o: OrderExpiryOrder, i: number) => {
        const start = o.createdAt;
        const end = o.status === "cancelled" ? (o.cancelledAt ?? now) : now;
        const startX = x(start);
        const endX = x(end);
        const rowY = y(i);
        const bidColor = "var(--pos)";
        const offerColor = "var(--neg)";
        const color = o.side === "BID" ? bidColor : offerColor;
        const barH = Math.max(4, rowHeight * 0.55);

        // Overlay a "past TTL" region (dashed) so the viewer sees when the
        // order became eligible for cancellation.
        const ttlBoundary = start + ttlMs;
        const ttlX = x(Math.min(ttlBoundary, tMax));
        const showTtl = ttlBoundary > start && ttlBoundary < end + 500;

        return (
          <g key={o.seq}>
            {/* Row background */}
            <rect x={PAD_L} y={rowY - barH / 2} width={W - PAD_L - PAD_R} height={barH} fill="transparent" />

            {/* Bar for the "alive" portion (before TTL) */}
            <rect
              x={startX}
              y={rowY - barH / 2}
              width={Math.max(1, (showTtl ? ttlX : endX) - startX)}
              height={barH}
              fill={color}
              opacity={0.55}
            />

            {/* Bar for the "past-TTL" portion — same color but dashed / lighter */}
            {showTtl && endX > ttlX && (
              <rect
                x={ttlX}
                y={rowY - barH / 2}
                width={Math.max(1, endX - ttlX)}
                height={barH}
                fill={color}
                opacity={0.25}
                stroke={color}
                strokeDasharray="3,2"
                strokeWidth={0.8}
              />
            )}

            {/* Creation tick */}
            <line x1={startX} x2={startX} y1={rowY - barH / 2} y2={rowY + barH / 2} stroke={color} strokeWidth={1.4} />

            {/* Cancel × */}
            {o.status === "cancelled" && (
              <g stroke="#ff6b7a" strokeWidth={1.6}>
                <line x1={endX - 4} x2={endX + 4} y1={rowY - 4} y2={rowY + 4} />
                <line x1={endX - 4} x2={endX + 4} y1={rowY + 4} y2={rowY - 4} />
              </g>
            )}

            {/* Order label at left */}
            <text
              x={PAD_L - 8}
              y={rowY + 3}
              fill={o.status === "cancelled" ? "var(--text-faint)" : color}
              fontSize={9}
              textAnchor="end"
              fontFamily="ui-monospace, monospace"
            >
              #{o.seq} {o.side}
            </text>
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
        <text x={200} y={-2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">dashed = past TTL, eligible</text>
      </g>
    </svg>
  );
}
