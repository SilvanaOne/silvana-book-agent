"use client";

import { VENUES } from "@/lib/rebalance-engine";
import type { Snapshot } from "@/lib/store";

function money(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return n.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

type Props = Readonly<{ snap: Snapshot }>;

export function InfoGrid({ snap }: Props) {
  const { analysis, plan, job, portfolio } = snap;
  const jobPhase = job ? job.phase : "none";
  const openTransfers = job && job.phase !== "completed" ? job.steps.length : 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">agent</span><span className="v">batch-order-management</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">execution</span><span className="v">plan_only</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PORTFOLIO</div></div>
        <div className="kv-row"><span className="k">name</span><span className="v">{portfolio.name}</span></div>
        <div className="kv-row"><span className="k">NAV</span><span className="v accent">{money(analysis.nav)} {portfolio.quoteCurrency}</span></div>
        <div className="kv-row"><span className="k">quote ccy</span><span className="v">{portfolio.quoteCurrency}</span></div>
        <div className="kv-row"><span className="k">assets</span><span className="v">{analysis.rows.length}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">DRIFT</div></div>
        <div className="kv-row"><span className="k">threshold</span><span className="v accent">±{analysis.thresholdBps} bps</span></div>
        <div className="kv-row"><span className="k">max drift</span><span className="v">{Math.round(analysis.maxAbsDriftBps)} bps</span></div>
        <div className="kv-row"><span className="k">breaches</span><span className={`v ${analysis.breaches > 0 ? "accent" : ""}`}>{analysis.breaches}</span></div>
        <div className="kv-row"><span className="k">in band</span><span className="v">{analysis.rows.length - analysis.breaches}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">JOB</div></div>
        <div className="kv-row"><span className="k">status</span><span className={`v ${jobPhase === "running" || jobPhase === "queued" ? "accent" : ""}`}>{jobPhase}</span></div>
        <div className="kv-row"><span className="k">job id</span><span className="v">{job ? job.id : "—"}</span></div>
        <div className="kv-row"><span className="k">progress</span><span className="v">{job ? `${job.progressPct}%` : "—"}</span></div>
        <div className="kv-row"><span className="k">open transfers</span><span className="v">{openTransfers}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VENUES</div></div>
        {VENUES.map((v) => (
          <div key={v.id} className="kv-row"><span className="k">{v.label}</span><span className="v">{v.role}</span></div>
        ))}
        <div className="kv-row"><span className="k">routing</span><span className="v">round-robin</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PLAN</div></div>
        <div className="kv-row"><span className="k">preview</span><span className="v">{plan ? "ready" : "—"}</span></div>
        <div className="kv-row"><span className="k">orders</span><span className="v">{plan ? plan.orders.length : 0}</span></div>
        <div className="kv-row"><span className="k">est. notional</span><span className="v accent">{plan ? money(plan.estimatedNotional) : "—"}</span></div>
        <div className="kv-row"><span className="k">audit entries</span><span className="v">{snap.audit.length}</span></div>
      </div>
    </div>
  );
}
