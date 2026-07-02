"use client";

import type { ReadinessCheckState } from "@/lib/readinesscheck-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ readinesscheck: ReadinessCheckState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ readinesscheck, walk }: Props) {
  const c = readinesscheck?.config;
  const stateLabel = readinesscheck === null
    ? "idle"
    : readinesscheck.status === "monitoring"
      ? (readinesscheck.checksCount === 0 ? "warming" : readinesscheck.overallReady ? "READY" : "NOT READY")
      : "stopped";

  const now = Date.now();
  const nextCheckInSec = readinesscheck && c && readinesscheck.lastCheckAt !== null
    ? Math.max(0, Math.ceil((readinesscheck.lastCheckAt + c.checkIntervalSecs * 1000 - now) / 1000))
    : null;

  const readyClass = readinesscheck?.overallReady ? "positive" : readinesscheck && readinesscheck.checksCount > 0 ? "negative" : "";

  const timelineLabel = readinesscheck === null
    ? "not started"
    : `${readinesscheck.checksCount} check${readinesscheck.checksCount === 1 ? "" : "s"} @ ${c?.checkIntervalSecs ?? "?"}s`;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">readiness-check</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RC CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">required balances</span><span className="v">{c.requiredBalances.length}</span></div>
            <div className="kv-row"><span className="k">max failed</span><span className="v">{c.maxFailedSettlements}</span></div>
            <div className="kv-row"><span className="k">max pending</span><span className="v">{c.maxPendingSettlements}</span></div>
            <div className="kv-row"><span className="k">preapproval req</span><span className="v">{c.requirePreapproval ? "yes" : "no"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">required balances</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max failed</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max pending</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">preapproval req</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CURRENT STATE</div></div>
        {readinesscheck && c ? (
          <>
            {readinesscheck.currentBalances.slice(0, 4).map((b) => {
              const req = c.requiredBalances.find((r) => r.instrument === b.instrument);
              const ok = req ? b.balance >= req.minAmount : true;
              return (
                <div key={b.instrument} className="kv-row">
                  <span className="k">{b.instrument}</span>
                  <span className={`v ${req ? (ok ? "" : "negative") : "faint"}`}>{fmt(b.balance)}{req ? ` / ${req.minAmount}` : ""}</span>
                </div>
              );
            })}
            {readinesscheck.currentBalances.length === 0 && (
              <div className="kv-row"><span className="k">—</span><span className="v faint">no balances</span></div>
            )}
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">balances</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">&nbsp;</span><span className="v faint">&nbsp;</span></div>
            <div className="kv-row"><span className="k">&nbsp;</span><span className="v faint">&nbsp;</span></div>
            <div className="kv-row"><span className="k">&nbsp;</span><span className="v faint">&nbsp;</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SETTLEMENTS</div></div>
        <div className="kv-row"><span className="k">failed</span><span className={`v ${readinesscheck && c && readinesscheck.failedSettlements > c.maxFailedSettlements ? "negative" : ""}`}>{readinesscheck?.failedSettlements ?? "—"}</span></div>
        <div className="kv-row"><span className="k">pending</span><span className={`v ${readinesscheck && c && readinesscheck.pendingSettlements > c.maxPendingSettlements ? "negative" : ""}`}>{readinesscheck?.pendingSettlements ?? "—"}</span></div>
        <div className="kv-row"><span className="k">preapproval</span><span className={`v ${readinesscheck && c?.requirePreapproval && !readinesscheck.preapproval ? "negative" : readinesscheck?.preapproval ? "positive" : ""}`}>{readinesscheck === null ? "—" : readinesscheck.preapproval ? "present" : "missing"}</span></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(readinesscheck?.currentPrice)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATUS</div></div>
        <div className="kv-row"><span className="k">overall</span><span className={`v ${readyClass}`}>{stateLabel}</span></div>
        <div className="kv-row"><span className="k">next check in</span><span className="v">{nextCheckInSec === null ? "—" : `${nextCheckInSec}s`}</span></div>
        <div className="kv-row"><span className="k">checks total</span><span className="v">{readinesscheck?.checksCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">transitions</span><span className="v accent">{readinesscheck?.transitionsCount ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TIMELINE</div></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{timelineLabel}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
