"use client";

import type { InventoryMgmtState } from "@/lib/inventorymgmt-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function sgn(n: number | null | undefined, digits = 3): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(digits)}`;
}

type Props = Readonly<{ inventorymgmt: InventoryMgmtState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ inventorymgmt, walk }: Props) {
  const c = inventorymgmt?.config;
  const bal = inventorymgmt?.currentBalance ?? null;
  const upper = c ? c.target + c.tolerance : null;
  const lower = c ? c.target - c.tolerance : null;
  const inBand = c && bal !== null ? bal >= (lower as number) && bal <= (upper as number) : null;
  const deviation = c && bal !== null ? bal - c.target : null;
  const currentPct = c && bal !== null && c.target > 0 ? (bal / c.target) * 100 : null;
  const openOrders = inventorymgmt?.orders.filter((o) => o.status === "open") ?? [];
  const openCount = openOrders.length;
  const lastOrder = inventorymgmt?.orders[inventorymgmt.orders.length - 1];
  const stateLabel = inventorymgmt === null ? "idle" : inventorymgmt.status === "monitoring" ? (inBand ? "in-band" : "rebalancing") : "stopped";
  const bandClass = inBand === false ? "negative" : inBand === true ? "positive" : "";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">inventory-mgmt</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">check cadence</span><span className="v">{c ? `${c.checkIntervalSecs}s` : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">IM CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">instrument</span><span className="v">{c.instrument}</span></div>
            <div className="kv-row"><span className="k">target</span><span className="v accent">{fmt(c.target)}</span></div>
            <div className="kv-row"><span className="k">tolerance</span><span className="v">±{fmt(c.tolerance)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">instrument</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">target</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">tolerance</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">balance</span><span className="v">{fmt(bal)}</span></div>
        <div className="kv-row"><span className="k">mid price</span><span className="v">{fmt(inventorymgmt?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">in-band</span><span className={`v ${bandClass}`}>{inBand === null ? "—" : inBand ? "yes" : "no"}</span></div>
        <div className="kv-row"><span className="k">deviation</span><span className="v">{sgn(deviation, 3)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TARGET ZONE</div></div>
        <div className="kv-row"><span className="k">upper bound</span><span className="v">{fmt(upper)}</span></div>
        <div className="kv-row"><span className="k">lower bound</span><span className="v">{fmt(lower)}</span></div>
        <div className="kv-row"><span className="k">current pct</span><span className="v">{currentPct === null ? "—" : `${currentPct.toFixed(1)}%`}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v accent">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ORDERS</div></div>
        <div className="kv-row"><span className="k">open</span><span className="v">{openCount}</span></div>
        <div className="kv-row"><span className="k">placed</span><span className="v">{inventorymgmt?.ordersPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">filled</span><span className="v accent">{inventorymgmt?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">last</span><span className="v">{lastOrder ? `${lastOrder.side} ${fmt(lastOrder.qty)} @ ${fmt(lastOrder.price)}` : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">offset</span><span className="v">{c ? `${sgn(c.priceOffsetPct, 3)}%` : "—"}</span></div>
      </div>
    </div>
  );
}
