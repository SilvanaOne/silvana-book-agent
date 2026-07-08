"use client";

import type { Tick } from "@/lib/store";
import type { DcaPortfolioState, MarketProgress, DcaOrder } from "@/lib/dcaportfolio-engine";

type Props = Readonly<{ ticks: readonly Tick[]; dcaportfolio: DcaPortfolioState | null }>;

const SPARK_W = 360;
const SPARK_H = 140;
const PAD_L = 44;
const PAD_R = 60;
const PAD_T = 12;
const PAD_B = 22;

export function DcaPortfolioChart({ ticks, dcaportfolio }: Props) {
  if (ticks.length === 0 || !dcaportfolio) {
    return (
      <div
        style={{
          height: 200,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          color: "var(--text-faint)",
        }}
      >
        No data — start DCA Portfolio to see per-market mid and order placements.
      </div>
    );
  }

  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: dcaportfolio.progress.length > 1 ? "1fr 1fr" : "1fr",
        gap: 12,
      }}
    >
      {dcaportfolio.progress.map((mp) => (
        <MarketSpark key={mp.market} mp={mp} ticks={ticks} />
      ))}
    </div>
  );
}

function MarketSpark({ mp, ticks }: { mp: MarketProgress; ticks: readonly Tick[] }) {
  // Build the per-market price series from ticks
  const series: { t: number; price: number }[] = [];
  for (const tk of ticks) {
    const p = tk.prices[mp.market];
    if (typeof p === "number" && p > 0) series.push({ t: tk.t, price: p });
  }
  if (series.length === 0) {
    return (
      <div className="card" style={{ padding: 12 }}>
        <div className="mono" style={{ fontSize: 12, color: "var(--text-faint)" }}>{mp.market}</div>
        <div style={{ height: SPARK_H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
          waiting for first tick…
        </div>
      </div>
    );
  }

  const prices = series.map((s) => s.price);
  const orderPrices = mp.orders.map((o) => o.price);
  const orderMids = mp.orders.map((o) => o.mid);
  const minAll = Math.min(...prices, ...orderPrices, ...orderMids);
  const maxAll = Math.max(...prices, ...orderPrices, ...orderMids);
  const pad = (maxAll - minAll) * 0.15 || minAll * 0.02 || 0.01;
  const yMin = minAll - pad;
  const yMax = maxAll + pad;
  const yRange = yMax - yMin || 1;
  const tMin = series[0].t;
  const tMax = series[series.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (SPARK_W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (SPARK_H - PAD_T - PAD_B);

  const pricePath = series
    .map((s, i) => `${i === 0 ? "M" : "L"}${x(s.t).toFixed(1)},${y(s.price).toFixed(1)}`)
    .join(" ");

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [yMin, yMin + yRange / 2, yMax];

  const lastPrice = series[series.length - 1].price;
  const avg = mp.avgPrice;

  return (
    <div className="card" style={{ padding: 12 }}>
      <div className="row" style={{ justifyContent: "space-between", alignItems: "baseline", marginBottom: 6 }}>
        <div className="mono" style={{ fontSize: 13, fontWeight: 600 }}>{mp.market}{mp.completed ? " · done" : ""}</div>
        <div className="mono" style={{ fontSize: 11, color: "var(--text-faint)" }}>
          orders {mp.orderCount} · avg {avg > 0 ? avg.toFixed(digits) : "—"} · qty {mp.totalQty.toFixed(6)}
        </div>
      </div>
      <svg viewBox={`0 0 ${SPARK_W} ${SPARK_H}`} style={{ width: "100%", height: "auto", maxHeight: SPARK_H + 10 }}>
        {yTicks.map((v, i) => (
          <g key={i}>
            <line x1={PAD_L} x2={SPARK_W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
            <text
              x={PAD_L - 6}
              y={y(v) + 3}
              fill="var(--text-faint)"
              fontSize={9}
              textAnchor="end"
              fontFamily="ui-monospace, monospace"
            >
              {v.toFixed(digits)}
            </text>
          </g>
        ))}

        <path d={pricePath} stroke="#ececf1" strokeWidth={1.4} fill="none" />

        {mp.orders.map((o: DcaOrder) => {
          const color = o.type === "BID" ? "var(--pos)" : "var(--neg)";
          return (
            <g key={o.seq}>
              <circle cx={x(o.t)} cy={y(o.price)} r={3.8} fill={color} stroke={color} strokeWidth={1} opacity={0.9} />
            </g>
          );
        })}

        <circle cx={x(tMax)} cy={y(lastPrice)} r={2.8} fill="#ececf1" />
        <text
          x={SPARK_W - PAD_R + 4}
          y={y(lastPrice) + 3}
          fill="var(--text-faint)"
          fontSize={10}
          fontFamily="ui-monospace, monospace"
        >
          {lastPrice.toFixed(digits)}
        </text>
        {avg > 0 && (
          <>
            <line
              x1={PAD_L}
              x2={SPARK_W - PAD_R}
              y1={y(avg)}
              y2={y(avg)}
              stroke="var(--accent)"
              strokeDasharray="3,3"
              strokeWidth={1}
              opacity={0.7}
            />
            <text
              x={SPARK_W - PAD_R + 4}
              y={y(avg) + 3}
              fill="var(--accent)"
              fontSize={9}
              fontFamily="ui-monospace, monospace"
            >
              avg
            </text>
          </>
        )}
      </svg>
    </div>
  );
}
