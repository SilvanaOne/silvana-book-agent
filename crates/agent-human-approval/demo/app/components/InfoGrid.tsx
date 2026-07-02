"use client";

import type { HumanApprovalState } from "@/lib/humanapproval-engine";

function fmt(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

type Props = Readonly<{ humanapproval: HumanApprovalState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ humanapproval, walk: _walk }: Props) {
  const c = humanapproval?.config;
  const now = Date.now();
  const queue = humanapproval?.queue ?? [];
  const oldest = queue.length > 0 ? Math.floor((now - queue[0].receivedAt) / 1000) : 0;
  const totalPendingNotional = queue.reduce((s, o) => s + o.notional, 0);

  const history = humanapproval?.history ?? [];
  const recent = history.slice(-3).reverse();
  const stateLabel = humanapproval === null ? "idle" : humanapproval.status === "monitoring" ? "monitoring" : "stopped";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">human-approval</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">HA CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">order arrival</span><span className="v">{c.orderArrivalPerTick}/tick</span></div>
            <div className="kv-row"><span className="k">auto approval</span><span className="v accent">{c.autoApprovalEnabled ? "enabled" : "off"}</span></div>
            <div className="kv-row"><span className="k">threshold</span><span className="v">{fmt(c.autoApprovalThreshold)}</span></div>
            <div className="kv-row"><span className="k">reviewer</span><span className="v">{c.reviewerName}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">order arrival</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">auto approval</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">threshold</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">reviewer</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">QUEUE</div></div>
        <div className="kv-row"><span className="k">pending count</span><span className="v accent">{humanapproval?.stats.pending ?? 0}</span></div>
        <div className="kv-row"><span className="k">oldest age</span><span className="v">{queue.length > 0 ? `${oldest}s` : "—"}</span></div>
        <div className="kv-row"><span className="k">notional pending</span><span className="v">{fmt(totalPendingNotional)}</span></div>
        <div className="kv-row"><span className="k">received total</span><span className="v">{humanapproval?.stats.totalReceived ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">DECISIONS</div></div>
        <div className="kv-row"><span className="k">approved</span><span className="v positive">{humanapproval?.stats.approved ?? 0}</span></div>
        <div className="kv-row"><span className="k">rejected</span><span className="v negative">{humanapproval?.stats.rejected ?? 0}</span></div>
        <div className="kv-row"><span className="k">auto-approved</span><span className="v">{humanapproval?.stats.autoApproved ?? 0}</span></div>
        <div className="kv-row"><span className="k">submitted</span><span className="v accent">{humanapproval?.stats.submitted ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RECENT</div></div>
        {recent.length === 0 ? (
          <div className="kv-row"><span className="k">no decisions yet</span><span className="v faint">—</span></div>
        ) : (
          recent.map((r) => {
            const cls = r.status === "approved" ? "positive" : "negative";
            const label = r.decidedBy === "auto" ? "auto" : r.decidedBy ?? "?";
            return (
              <div className="kv-row" key={r.id}>
                <span className="k mono" style={{ fontSize: 11 }}>{r.side} {r.quantity}@{fmt(r.price)}</span>
                <span className={`v ${cls}`}>{r.status} · {label}</span>
              </div>
            );
          })
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(humanapproval?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">market</span><span className="v">{c?.market ?? "—"}</span></div>
        <div className="kv-row"><span className="k">history len</span><span className="v">{history.length}</span></div>
      </div>
    </div>
  );
}
