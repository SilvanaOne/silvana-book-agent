"use client";

import type { InfiniteGridState } from "@/lib/infinitegrid-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function pctPlain(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return `${n.toFixed(3)}%`;
}

type Props = Readonly<{ infinitegrid: InfiniteGridState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ infinitegrid, walk }: Props) {
  const c = infinitegrid?.config;
  const openOrders = infinitegrid?.activeOrders.filter((o) => o.status === "open").length ?? 0;
  const stateLabel = infinitegrid === null ? "idle" : infinitegrid.gridCenter === null ? "building" : infinitegrid.status === "monitoring" ? "monitoring" : "stopped";
  const pnl = infinitegrid?.realizedPnl ?? 0;
  const pnlClass = pnl > 0 ? "positive" : pnl < 0 ? "negative" : "";
  const netInv = infinitegrid?.netInventory ?? 0;
  const invClass = netInv > 0 ? "positive" : netInv < 0 ? "negative" : "";

  let nextCheckIn: string = "—";
  if (infinitegrid && c && infinitegrid.lastRefreshAt > 0) {
    const rem = Math.max(0, Math.round((infinitegrid.lastRefreshAt + c.refreshSecs * 1000 - Date.now()) / 1000));
    nextCheckIn = `${rem}s`;
  } else if (infinitegrid && c) {
    nextCheckIn = `${c.refreshSecs}s`;
  }

  const stepAtCeiling = infinitegrid !== null && c !== undefined && infinitegrid!.currentStepPct >= c!.maxStepPct - 1e-9;
  const stepAtFloor = infinitegrid !== null && c !== undefined && infinitegrid!.currentStepPct <= c!.minStepPct + 1e-9;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">infinite-grid</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">GRID CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">levels / side</span><span className="v">{c.levels}</span></div>
            <div className="kv-row"><span className="k">qty / level</span><span className="v">{c.quantityPerLevel}</span></div>
            <div className="kv-row"><span className="k">step bounds</span><span className="v">{c.minStepPct}% – {c.maxStepPct}%</span></div>
            <div className="kv-row"><span className="k">σ multiplier</span><span className="v">×{c.stepMultiplier}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">levels / side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">qty / level</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">step bounds</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">σ multiplier</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(infinitegrid?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">grid center</span><span className="v accent">{fmt(infinitegrid?.gridCenter ?? null)}</span></div>
        <div className="kv-row"><span className="k">sigma (samples)</span><span className="v">{infinitegrid ? infinitegrid.sigma.toFixed(5) : "—"} ({infinitegrid?.logReturns.length ?? 0})</span></div>
        <div className="kv-row"><span className="k">raw step</span><span className="v">{pctPlain(infinitegrid?.rawStepPct)}</span></div>
        <div className="kv-row"><span className="k">clamped step</span><span className={`v accent ${stepAtCeiling ? "negative" : stepAtFloor ? "faint" : ""}`}>{pctPlain(infinitegrid?.currentStepPct)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">REFRESH</div></div>
        <div className="kv-row"><span className="k">refresh</span><span className="v">{c ? `${c.refreshSecs}s` : "—"}</span></div>
        <div className="kv-row"><span className="k">vol window</span><span className="v">{c?.volWindow ?? "—"}</span></div>
        <div className="kv-row"><span className="k">next check in</span><span className="v">{nextCheckIn}</span></div>
        <div className="kv-row"><span className="k">rebuilds</span><span className="v accent">{infinitegrid?.rebuildsCount ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FILLS</div></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{openOrders}</span></div>
        <div className="kv-row"><span className="k">orders placed</span><span className="v">{infinitegrid?.ordersPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders filled</span><span className="v accent">{infinitegrid?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">last fill</span><span className="v mono" style={{ fontSize: 11 }}>{infinitegrid?.lastFillMessage ?? "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">P&L (SIM)</div></div>
        <div className="kv-row"><span className="k">realized pnl</span><span className={`v ${pnlClass}`}>{fmt(pnl)}</span></div>
        <div className="kv-row"><span className="k">net inventory</span><span className={`v ${invClass}`}>{fmt(netInv)}</span></div>
        <div className="kv-row"><span className="k">rebuilds count</span><span className="v">{infinitegrid?.rebuildsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">walk drift/vol</span><span className="v">{walk.driftPerTick} / {(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
