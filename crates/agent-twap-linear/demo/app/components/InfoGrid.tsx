"use client";

import type { TwapState } from "@/lib/twap-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ twap: TwapState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ twap, walk }: Props) {
  const c = twap?.config;
  const now = Date.now();

  const stateLabel = twap === null
    ? "idle"
    : twap.status === "monitoring"
      ? "running"
      : twap.status === "completed"
        ? "completed"
        : "stopped";

  const nextInMs = twap && twap.status === "monitoring" && twap.slicesPlaced < (c?.slices ?? 0)
    ? Math.max(0, twap.nextSliceAt - now)
    : null;

  const pct = twap && c ? (twap.slicesPlaced / c.slices) * 100 : 0;
  const skipped = twap?.orders.filter((o) => o.skipped).length ?? 0;
  const placed = (twap?.slicesPlaced ?? 0) - skipped;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">twap</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TWAP CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">side</span><span className={`v ${c.side === "buy" ? "positive" : "negative"}`}>{c.side.toUpperCase()}</span></div>
            <div className="kv-row"><span className="k">total qty</span><span className="v accent">{c.total}</span></div>
            <div className="kv-row"><span className="k">slices</span><span className="v">{c.slices}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">total qty</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">slices</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(twap?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">slices placed</span><span className="v accent">{twap?.slicesPlaced ?? 0}{c ? ` / ${c.slices}` : ""}</span></div>
        <div className="kv-row"><span className="k">next slice in</span><span className="v">{nextInMs === null ? "—" : `${(nextInMs / 1000).toFixed(1)}s`}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PROGRESS</div></div>
        <div className="kv-row"><span className="k">completion</span><span className="v accent">{pct.toFixed(1)}%</span></div>
        <div className="kv-row"><span className="k">slice qty</span><span className="v">{twap ? twap.sliceQty : "—"}</span></div>
        <div className="kv-row"><span className="k">interval</span><span className="v">{twap ? `${(twap.intervalMs / 1000).toFixed(2)}s` : "—"}</span></div>
        <div className="kv-row"><span className="k">skipped</span><span className={`v ${skipped > 0 ? "negative" : ""}`}>{skipped}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICING</div></div>
        <div className="kv-row"><span className="k">offset from mid</span><span className="v">{c ? `${c.priceOffsetPct >= 0 ? "+" : ""}${c.priceOffsetPct}%` : "—"}</span></div>
        <div className="kv-row"><span className="k">limit price</span><span className="v">{c && c.limitPrice !== null ? fmt(c.limitPrice) : "none"}</span></div>
        <div className="kv-row"><span className="k">avg fill</span><span className="v accent">{twap && twap.totalPlaced > 0 ? fmt(twap.avgPrice) : "—"}</span></div>
        <div className="kv-row"><span className="k">placed qty</span><span className="v">{twap ? twap.totalPlaced : 0} / {placed} orders</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">seed</span><span className="v">{c ? fmt(c.startingPrice) : "—"}</span></div>
      </div>
    </div>
  );
}
