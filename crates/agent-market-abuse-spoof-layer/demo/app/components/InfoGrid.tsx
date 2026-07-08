"use client";

import type { MarketAbuseState } from "@/lib/marketabuse-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function relTime(t: number | undefined, now: number): string {
  if (!t) return "—";
  const dt = Math.max(0, now - t);
  if (dt < 1000) return "just now";
  const s = Math.floor(dt / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  return `${m}m ${s % 60}s ago`;
}

type Props = Readonly<{ marketabuse: MarketAbuseState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ marketabuse, walk }: Props) {
  const c = marketabuse?.config;
  const now = Date.now();
  const armed = marketabuse !== null && marketabuse.status === "monitoring";
  const stateLabel = marketabuse === null ? "idle" : armed ? "armed" : "stopped";
  const activeCount = marketabuse?.activeOrders.length ?? 0;
  const lastAlert = marketabuse?.alerts[marketabuse.alerts.length - 1];
  const recentAlerts = (marketabuse?.alerts ?? []).slice(-3).reverse();
  const total = (marketabuse?.spoofsDetected ?? 0) + (marketabuse?.layersDetected ?? 0);
  const detectionRate = marketabuse && marketabuse.ticksElapsed > 0 ? (total / marketabuse.ticksElapsed) : 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">market-abuse</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">ticks elapsed</span><span className="v">{marketabuse?.ticksElapsed ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MA CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">spoof burst / window</span><span className="v accent">≥{c.spoofBurst} in {c.spoofBurstWindowSecs}s</span></div>
            <div className="kv-row"><span className="k">spoof window</span><span className="v">≤{c.spoofWindowSecs}s</span></div>
            <div className="kv-row"><span className="k">layer thresholds</span><span className="v accent">≥{c.layerMinOrders} · {c.layerPriceBandPct}%</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">spoof burst / window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">spoof window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">layer thresholds</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">active orders</span><span className="v">{activeCount}</span></div>
        <div className="kv-row"><span className="k">cancelled orders</span><span className="v">{marketabuse?.ordersCancelled ?? 0}</span></div>
        <div className="kv-row"><span className="k">last alert</span><span className={`v ${lastAlert ? (lastAlert.kind === "spoof" ? "accent" : "negative") : ""}`}>{lastAlert ? lastAlert.kind : "—"}</span></div>
        <div className="kv-row"><span className="k">last alert age</span><span className="v">{relTime(marketabuse?.lastAlertAt, now)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">DETECTION</div></div>
        <div className="kv-row"><span className="k">spoofs detected</span><span className="v accent">{marketabuse?.spoofsDetected ?? 0}</span></div>
        <div className="kv-row"><span className="k">layers detected</span><span className="v accent">{marketabuse?.layersDetected ?? 0}</span></div>
        <div className="kv-row"><span className="k">alerts / tick</span><span className="v">{detectionRate.toFixed(4)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ALERTS</div></div>
        {recentAlerts.length === 0 ? (
          <div className="kv-row"><span className="k">recent</span><span className="v faint">—</span></div>
        ) : (
          recentAlerts.map((a, i) => (
            <div className="kv-row" key={i}>
              <span className="k">{a.kind}</span>
              <span className="v" style={{ fontSize: 11, textAlign: "right", maxWidth: 220, overflow: "hidden", textOverflow: "ellipsis" }} title={a.details}>{a.details}</span>
            </div>
          ))
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(marketabuse?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
