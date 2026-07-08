"use client";

import type { HumanApprovalState, PendingOrder } from "@/lib/humanapproval-engine";

type Props = Readonly<{
  humanapproval: HumanApprovalState | null;
  onApprove: (id: string) => Promise<void> | void;
  onReject: (id: string) => Promise<void> | void;
}>;

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

function ageSecs(receivedAt: number, now: number): string {
  return Math.max(0, Math.floor((now - receivedAt) / 1000)) + "s";
}

function shortId(id: string): string {
  const parts = id.split("-");
  if (parts.length < 3) return id;
  return `${parts[0]}-${parts[parts.length - 1]}`;
}

export function HumanApprovalChart({ humanapproval, onApprove, onReject }: Props) {
  if (!humanapproval) {
    return <div style={{ height: 240, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Human Approval to see the pending queue.</div>;
  }
  const now = Date.now();
  const rows: PendingOrder[] = humanapproval.queue.slice(0, 10);
  const stats = humanapproval.stats;
  const maxCount = Math.max(1, stats.pending, stats.approved, stats.rejected);
  const barW = (n: number) => `${Math.round((n / maxCount) * 100)}%`;

  return (
    <div className="stack" style={{ gap: 14 }}>
      <div>
        <div className="row" style={{ justifyContent: "space-between", alignItems: "baseline", marginBottom: 6 }}>
          <div className="mono" style={{ fontSize: 12, color: "var(--text-faint)" }}>PENDING QUEUE (top {rows.length}/{humanapproval.queue.length})</div>
          <div className="mono" style={{ fontSize: 11, color: "var(--text-faint)" }}>reviewer: {humanapproval.config.reviewerName}</div>
        </div>
        {rows.length === 0 ? (
          <div className="muted" style={{ fontSize: 13, padding: "12px 0" }}>No pending orders — waiting for upstream agents to enqueue…</div>
        ) : (
          <div style={{ overflowX: "auto" }}>
            <table className="mono" style={{ width: "100%", borderCollapse: "collapse", fontSize: 12 }}>
              <thead>
                <tr style={{ color: "var(--text-faint)", textAlign: "left" }}>
                  <th style={cellHead}>id</th>
                  <th style={cellHead}>from</th>
                  <th style={cellHead}>side</th>
                  <th style={{ ...cellHead, textAlign: "right" }}>qty</th>
                  <th style={{ ...cellHead, textAlign: "right" }}>price</th>
                  <th style={{ ...cellHead, textAlign: "right" }}>notional</th>
                  <th style={{ ...cellHead, textAlign: "right" }}>age</th>
                  <th style={{ ...cellHead, textAlign: "right" }}>action</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((o) => {
                  const sideCls = o.side === "BID" ? "positive" : "negative";
                  return (
                    <tr key={o.id} style={{ borderTop: "1px solid #22222c" }}>
                      <td style={cell}>{shortId(o.id)}</td>
                      <td style={cell}>{o.submittedBy}</td>
                      <td style={cell}><span className={sideCls}>{o.side}</span></td>
                      <td style={{ ...cell, textAlign: "right" }}>{o.quantity}</td>
                      <td style={{ ...cell, textAlign: "right" }}>{fmt(o.price)}</td>
                      <td style={{ ...cell, textAlign: "right" }}>{fmt(o.notional)}</td>
                      <td style={{ ...cell, textAlign: "right" }}>{ageSecs(o.receivedAt, now)}</td>
                      <td style={{ ...cell, textAlign: "right" }}>
                        <button className="primary" style={btnMini} onClick={() => onApprove(o.id)}>Approve</button>
                        <button className="danger" style={{ ...btnMini, marginLeft: 4 }} onClick={() => onReject(o.id)}>Reject</button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>

      <div>
        <div className="mono" style={{ fontSize: 12, color: "var(--text-faint)", marginBottom: 6 }}>DECISION STATS</div>
        <div className="stack" style={{ gap: 6 }}>
          <StatBar label="pending" count={stats.pending} width={barW(stats.pending)} color="var(--accent)" />
          <StatBar label="approved" count={stats.approved} width={barW(stats.approved)} color="var(--pos)" />
          <StatBar label="rejected" count={stats.rejected} width={barW(stats.rejected)} color="var(--neg)" />
        </div>
      </div>
    </div>
  );
}

function StatBar({ label, count, width, color }: { label: string; count: number; width: string; color: string }) {
  return (
    <div className="row" style={{ alignItems: "center", gap: 8 }}>
      <div className="mono" style={{ fontSize: 11, width: 74, color: "var(--text-faint)" }}>{label}</div>
      <div style={{ flex: 1, background: "#22222c", height: 10, borderRadius: 3, overflow: "hidden" }}>
        <div style={{ width, background: color, height: "100%" }} />
      </div>
      <div className="mono" style={{ fontSize: 11, width: 44, textAlign: "right" }}>{count}</div>
    </div>
  );
}

const cellHead: React.CSSProperties = { padding: "4px 6px", fontWeight: "normal", fontSize: 11 };
const cell: React.CSSProperties = { padding: "4px 6px", whiteSpace: "nowrap" };
const btnMini: React.CSSProperties = { padding: "2px 8px", fontSize: 11 };
