"use client";

import type { KillswitchState } from "@/lib/killswitch-engine";

function fmtSecs(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "—";
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  const rem = s % 60;
  return `${m}m ${rem}s`;
}

function pct(n: number): string {
  const sign = n >= 0 ? "+" : "";
  return `${sign}${n.toFixed(1)}%`;
}

type Props = Readonly<{ killswitch: KillswitchState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ killswitch, walk: _walk }: Props) {
  const c = killswitch?.config;
  const armed = killswitch?.status === "monitoring";
  const tripped = killswitch?.status === "tripped";
  const stateLabel = killswitch === null ? "idle" : killswitch.status;

  const now = Date.now();
  const uptime = killswitch ? now - killswitch.startedAt : null;
  const nextCheckIn = armed && c
    ? Math.max(0, c.checkIntervalSecs - (killswitch!.ticksElapsed % Math.max(1, Math.floor(c.checkIntervalSecs))))
    : null;

  const distOpen = c && killswitch
    ? ((c.maxOpenOrders - killswitch.openOrders) / Math.max(1, c.maxOpenOrders)) * 100
    : null;
  const distFail = c && killswitch
    ? ((c.maxFailedSettlements - killswitch.failedSettlements) / Math.max(1, c.maxFailedSettlements)) * 100
    : null;

  const openWarn = c && killswitch && killswitch.openOrders > c.maxOpenOrders * 0.85;
  const failWarn = c && killswitch && killswitch.failedSettlements > c.maxFailedSettlements * 0.85;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">killswitch</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">KS CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">max open orders</span><span className="v accent">{c.maxOpenOrders}</span></div>
            <div className="kv-row"><span className="k">max failed</span><span className="v accent">{c.maxFailedSettlements}</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v">{c.checkIntervalSecs}s</span></div>
            <div className="kv-row"><span className="k">failure rate/tick</span><span className="v">{(c.failureRatePerTick * 100).toFixed(1)}%</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">max open orders</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max failed</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">failure rate/tick</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">open orders</span><span className={`v ${openWarn ? "negative" : ""}`}>{killswitch?.openOrders ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed settlements</span><span className={`v ${failWarn ? "negative" : ""}`}>{killswitch?.failedSettlements ?? 0}</span></div>
        <div className="kv-row"><span className="k">armed</span><span className="v">{armed ? "yes" : "no"}</span></div>
        <div className="kv-row"><span className="k">tripped</span><span className={`v ${tripped ? "negative" : ""}`}>{tripped ? "YES" : "no"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">THRESHOLDS</div></div>
        <div className="kv-row"><span className="k">dist to open limit</span><span className={`v ${distOpen !== null && distOpen < 15 ? "negative" : ""}`}>{distOpen === null ? "—" : pct(distOpen)}</span></div>
        <div className="kv-row"><span className="k">dist to fail limit</span><span className={`v ${distFail !== null && distFail < 15 ? "negative" : ""}`}>{distFail === null ? "—" : pct(distFail)}</span></div>
        <div className="kv-row"><span className="k">open limit</span><span className="v">{c ? c.maxOpenOrders : "—"}</span></div>
        <div className="kv-row"><span className="k">fail limit</span><span className="v">{c ? c.maxFailedSettlements : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRIP</div></div>
        {tripped && killswitch ? (
          <>
            <div className="kv-row"><span className="k">reason</span><span className="v negative">{killswitch.breaches[0] ?? "—"}</span></div>
            {killswitch.breaches[1] && <div className="kv-row"><span className="k">reason 2</span><span className="v negative">{killswitch.breaches[1]}</span></div>}
            <div className="kv-row"><span className="k">orders cancelled</span><span className="v accent">{killswitch.ordersCancelled ?? 0}</span></div>
            <div className="kv-row"><span className="k">tripped at</span><span className="v mono">{killswitch.tripTimestamp ? new Date(killswitch.tripTimestamp).toLocaleTimeString() : "—"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">reason</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">orders cancelled</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">tripped at</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">exit code</span><span className="v faint">2 on trip</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATUS</div></div>
        <div className="kv-row"><span className="k">state</span><span className={`v ${tripped ? "negative" : armed ? "positive" : ""}`}>{stateLabel}</span></div>
        <div className="kv-row"><span className="k">next check in</span><span className="v">{armed && nextCheckIn !== null ? `${nextCheckIn}s` : "—"}</span></div>
        <div className="kv-row"><span className="k">ticks elapsed</span><span className="v">{killswitch?.ticksElapsed ?? 0}</span></div>
        <div className="kv-row"><span className="k">uptime</span><span className="v">{fmtSecs(uptime)}</span></div>
      </div>
    </div>
  );
}
