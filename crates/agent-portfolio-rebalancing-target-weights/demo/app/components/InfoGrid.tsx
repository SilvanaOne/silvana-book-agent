"use client";

import type { PortfolioRebalancingState } from "@/lib/portfoliorebalancing-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ portfoliorebalancing: PortfolioRebalancingState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ portfoliorebalancing, walk }: Props) {
  const c = portfoliorebalancing?.config;
  const openOrders = portfoliorebalancing?.orders.filter((o) => o.status === "open").length ?? 0;
  const anyBreach = (portfoliorebalancing?.weights ?? []).some(
    (w) => c && Math.abs(w.deviationPct) > c.thresholdPct,
  );
  const stateLabel = portfoliorebalancing === null
    ? "idle"
    : portfoliorebalancing.status !== "rebalancing"
      ? "stopped"
      : anyBreach ? "rebalancing" : "armed";
  const nextCheckIn = (() => {
    if (!portfoliorebalancing || !c) return null;
    if (portfoliorebalancing.lastCheckAt === null) return 0;
    const remaining = c.checkIntervalSecs * 1000 - (Date.now() - portfoliorebalancing.lastCheckAt);
    return Math.max(0, Math.round(remaining / 1000));
  })();

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">portfolio-rebalancing</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PR CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">targets</span><span className="v">{c.targets.length}</span></div>
            <div className="kv-row"><span className="k">threshold</span><span className="v accent">±{c.thresholdPct}pp</span></div>
            <div className="kv-row"><span className="k">rebalance fraction</span><span className="v">{c.rebalanceFraction}</span></div>
            <div className="kv-row"><span className="k">cycle</span><span className="v">{c.checkIntervalSecs}s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">targets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">threshold</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">rebalance fraction</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">cycle</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PORTFOLIO</div></div>
        <div className="kv-row"><span className="k">value (quote)</span><span className="v accent">{fmt(portfoliorebalancing?.portfolioValueQuote)}</span></div>
        <div className="kv-row"><span className="k">instruments</span><span className="v">{portfoliorebalancing?.balances.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">base mid</span><span className="v">{fmt(portfoliorebalancing?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">walk vol/tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">WEIGHTS</div></div>
        {portfoliorebalancing && portfoliorebalancing.weights.length > 0 ? (
          portfoliorebalancing.weights.map((w) => {
            const breach = c && Math.abs(w.deviationPct) > c.thresholdPct;
            return (
              <div key={w.instrument} className="kv-row">
                <span className="k">{w.instrument}</span>
                <span className={`v ${breach ? "accent" : ""}`}>
                  {(w.currentWeight * 100).toFixed(2)}% → {(w.targetWeight * 100).toFixed(2)}% ({w.deviationPct >= 0 ? "+" : ""}{w.deviationPct.toFixed(2)}pp)
                </span>
              </div>
            );
          })
        ) : (
          <div className="kv-row"><span className="k">weights</span><span className="v faint">—</span></div>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ORDERS</div></div>
        <div className="kv-row"><span className="k">open</span><span className="v">{openOrders}</span></div>
        <div className="kv-row"><span className="k">placed</span><span className="v">{portfoliorebalancing?.ordersPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">filled</span><span className="v accent">{portfoliorebalancing?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">cycles run</span><span className="v">{portfoliorebalancing?.cyclesRun ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">next check</span><span className="v">{nextCheckIn === null ? "—" : `${nextCheckIn}s`}</span></div>
        <div className="kv-row"><span className="k">any breach</span><span className={`v ${anyBreach ? "accent" : ""}`}>{portfoliorebalancing ? (anyBreach ? "yes" : "no") : "—"}</span></div>
        <div className="kv-row"><span className="k">drift/tick</span><span className="v">{walk.driftPerTick}</span></div>
      </div>
    </div>
  );
}
