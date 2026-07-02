"use client";

import type { RiskAlertState } from "@/lib/riskalert-engine";

function fmt(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  if (abs >= 1000) return n.toFixed(0);
  if (abs >= 1) return n.toFixed(2);
  return n.toFixed(4);
}

type Props = Readonly<{ riskalert: RiskAlertState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ riskalert, walk }: Props) {
  const c = riskalert?.config;
  const now = Date.now();
  const nextIn = riskalert?.nextCheckAt ? Math.max(0, Math.round((riskalert.nextCheckAt - now) / 1000)) : null;
  const activeCount = riskalert?.activeBreaches.length ?? 0;
  const lastKind = riskalert?.lastAlertKind ?? null;
  const lastValue = riskalert?.lastAlertValue ?? null;

  const stateLabel = riskalert === null
    ? "idle"
    : riskalert.status !== "monitoring" ? "stopped"
    : activeCount > 0 ? "breach" : "armed";
  const stateClass = stateLabel === "breach" ? "negative" : stateLabel === "armed" ? "positive" : "";

  // events / min (alerts count over runtime seconds; simple heuristic)
  let ratePerMin: number | null = null;
  if (riskalert && riskalert.alerts.length > 0) {
    const first = riskalert.alerts[0].t;
    const secs = Math.max(1, (now - first) / 1000);
    ratePerMin = (riskalert.alertsCount / secs) * 60;
  }

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">risk-alert</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RA CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">max open orders</span><span className="v accent">{c.maxOpenOrders}</span></div>
            <div className="kv-row"><span className="k">max failed settl</span><span className="v accent">{c.maxFailedSettlements}</span></div>
            <div className="kv-row"><span className="k">max open notional</span><span className="v accent">{fmt(c.maxOpenNotional)}</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v">{c.checkIntervalSecs}s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">max open orders</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max failed settl</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max open notional</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">open orders</span><span className={`v ${riskalert && c && riskalert.openOrders > c.maxOpenOrders ? "negative" : ""}`}>{riskalert?.openOrders ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed settl</span><span className={`v ${riskalert && c && riskalert.failedSettlements > c.maxFailedSettlements ? "negative" : ""}`}>{riskalert?.failedSettlements ?? 0}</span></div>
        <div className="kv-row"><span className="k">open notional</span><span className={`v ${riskalert && c && riskalert.openNotional > c.maxOpenNotional ? "negative" : ""}`}>{fmt(riskalert?.openNotional ?? 0)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className={`v ${stateClass}`}>{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BREACHES</div></div>
        <div className="kv-row"><span className="k">active count</span><span className={`v ${activeCount > 0 ? "negative" : ""}`}>{activeCount}</span></div>
        <div className="kv-row"><span className="k">last kind</span><span className="v">{lastKind ?? "—"}</span></div>
        <div className="kv-row"><span className="k">last value</span><span className="v">{fmt(lastValue)}</span></div>
        <div className="kv-row"><span className="k">next check in</span><span className="v">{nextIn !== null ? `${nextIn}s` : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ALERTS</div></div>
        <div className="kv-row"><span className="k">total emitted</span><span className="v accent">{riskalert?.alertsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">last alert at</span><span className="v">{riskalert?.alerts.length ? new Date(riskalert.alerts[riskalert.alerts.length - 1].t).toLocaleTimeString() : "—"}</span></div>
        <div className="kv-row"><span className="k">rate per min</span><span className="v">{ratePerMin === null ? "—" : ratePerMin.toFixed(2)}</span></div>
        <div className="kv-row"><span className="k">action</span><span className="v">notify-only</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">THROUGHPUT</div></div>
        <div className="kv-row"><span className="k">order growth/tick</span><span className="v">{c ? c.orderGrowthPerTick : "—"}</span></div>
        <div className="kv-row"><span className="k">notional grow/tick</span><span className="v">{c ? fmt(c.notionalGrowthPerTick) : "—"}</span></div>
        <div className="kv-row"><span className="k">failure rate/tick</span><span className="v">{c ? c.failureRatePerTick : "—"}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
