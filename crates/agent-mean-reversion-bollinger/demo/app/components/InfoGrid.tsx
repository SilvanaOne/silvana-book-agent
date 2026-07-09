"use client";

import type { MrState } from "@/lib/mr-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ mr: MrState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ mr, walk }: Props) {
  const c = mr?.config;
  const inWarmup = mr && mr.samples < (c?.warmupSamples ?? 0);
  const openOrders = mr?.orders.filter((o) => o.status === "open").length ?? 0;
  const stateLabel = mr === null ? "idle" : inWarmup ? "warmup" : mr.status === "monitoring" ? "armed" : "stopped";
  const pnlClass = (mr?.realizedPnl ?? 0) > 0 ? "positive" : (mr?.realizedPnl ?? 0) < 0 ? "negative" : "";

  const armed = mr && !inWarmup;
  const above = armed && mr.upper !== null && mr.lastMid >= mr.upper;
  const below = armed && mr.lower !== null && mr.lastMid <= mr.lower;

  const lastSignal = mr && mr.orders.length > 0 ? mr.orders[mr.orders.length - 1] : null;
  const lastSignalLabel = lastSignal
    ? `${lastSignal.type} #${lastSignal.seq} @ ${fmt(lastSignal.price)} (${lastSignal.status})`
    : "—";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">mean-reversion / bollinger</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MR CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v">{c.window}</span></div>
            <div className="kv-row"><span className="k">k</span><span className="v accent">{c.k}</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v">{c.quantity}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">k</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(mr?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">sma</span><span className="v accent">{fmt(mr?.sma ?? null)}</span></div>
        <div className="kv-row"><span className="k">stddev</span><span className="v">{fmt(mr?.stddev ?? null)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BOLLINGER BANDS</div></div>
        <div className="kv-row"><span className="k">upper band</span><span className={`v ${above ? "accent" : ""}`}>{fmt(mr?.upper ?? null)}</span></div>
        <div className="kv-row"><span className="k">lower band</span><span className={`v ${below ? "accent" : ""}`}>{fmt(mr?.lower ?? null)}</span></div>
        <div className="kv-row"><span className="k">warmup progress</span><span className="v">{mr?.samples ?? 0}{c ? ` / ${c.warmupSamples}` : ""}</span></div>
        <div className="kv-row"><span className="k">last signal</span><span className="v">{lastSignalLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SIGNALS</div></div>
        <div className="kv-row"><span className="k">signals emitted</span><span className="v">{mr?.signals ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders placed</span><span className="v">{mr?.ordersPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders filled</span><span className="v accent">{mr?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{openOrders}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">P&L (SIM)</div></div>
        <div className="kv-row"><span className="k">realized pnl</span><span className={`v ${pnlClass}`}>{fmt(mr?.realizedPnl)}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
