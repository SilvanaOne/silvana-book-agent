"use client";

import type { FailureRecoveryState } from "@/lib/failurerecovery-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 6;
  return n.toFixed(digits);
}

type Props = Readonly<{ failurerecovery: FailureRecoveryState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ failurerecovery, walk }: Props) {
  const c = failurerecovery?.config;
  const stateLabel = failurerecovery === null ? "idle" : failurerecovery.status === "monitoring" ? "sweeping" : "stopped";

  const proposalsTotal = failurerecovery?.proposals.length ?? 0;
  const pendingCount = failurerecovery?.proposals.filter((p) => p.status === "pending").length ?? 0;
  const failedInBuf = failurerecovery?.proposals.filter((p) => p.status === "failed").length ?? 0;
  const oldestPending = failurerecovery
    ? failurerecovery.proposals.filter((p) => p.status === "pending").reduce((max, p) => Math.max(max, p.ageSec), 0)
    : 0;
  const activeRelated = failurerecovery?.relatedOrders.filter((o) => o.status === "active").length ?? 0;

  const tickCount = Math.max(1, failurerecovery?.tickCount ?? 1);
  const newRate = failurerecovery ? (failurerecovery.newProposalsCount / tickCount).toFixed(3) : "0";
  const settledRate = failurerecovery ? (failurerecovery.settledCount / tickCount).toFixed(3) : "0";
  const failedRate = failurerecovery ? (failurerecovery.failedCount / tickCount).toFixed(3) : "0";

  const oldestWarn = c && oldestPending > c.maxPendingAgeSecs;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">failure-recovery</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FR CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">max pending age</span><span className="v accent">{c.maxPendingAgeSecs}s</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v">{c.checkIntervalSecs}s</span></div>
            <div className="kv-row"><span className="k">cancel related</span><span className="v">{c.cancelRelatedOrders ? "yes" : "no"}</span></div>
            <div className="kv-row"><span className="k">dry-run</span><span className="v">{c.dryRun ? "yes" : "no"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">max pending age</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">cancel related</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">dry-run</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">proposals total</span><span className="v">{proposalsTotal}</span></div>
        <div className="kv-row"><span className="k">pending</span><span className="v accent">{pendingCount}</span></div>
        <div className="kv-row"><span className="k">failed (buffered)</span><span className="v">{failedInBuf}</span></div>
        <div className="kv-row"><span className="k">oldest pending age</span><span className={`v ${oldestWarn ? "negative" : ""}`}>{oldestPending}s</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">THROUGHPUT</div></div>
        <div className="kv-row"><span className="k">new / tick</span><span className="v">{newRate}</span></div>
        <div className="kv-row"><span className="k">settled / tick</span><span className="v">{settledRate}</span></div>
        <div className="kv-row"><span className="k">failed / tick</span><span className="v">{failedRate}</span></div>
        <div className="kv-row"><span className="k">active related orders</span><span className="v">{activeRelated}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SWEEP</div></div>
        <div className="kv-row"><span className="k">sweeps count</span><span className="v">{failurerecovery?.sweepsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">stale flagged</span><span className="v accent">{failurerecovery?.staleFlaggedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed found</span><span className="v">{failurerecovery?.failedFoundCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders cancelled</span><span className="v">{failurerecovery?.cancelledCount ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(failurerecovery?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">ticks observed</span><span className="v">{failurerecovery?.tickCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
