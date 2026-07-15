"use client";

import { useState } from "react";
import { shortHash } from "@/lib/rebalance-engine";
import type { DriftAnalysis } from "@/lib/rebalance-engine";
import type { RebalanceJob, RebalancePlan } from "@/lib/store";

type Props = Readonly<{
  analysis: DriftAnalysis;
  plan: RebalancePlan | null;
  job: RebalanceJob | null;
  onPreview: () => Promise<void>;
  onExecute: () => Promise<void>;
}>;

function Pipeline({ phase }: { phase: "queued" | "running" | "completed" }) {
  const order = ["queued", "running", "completed"] as const;
  const idx = order.indexOf(phase);
  return (
    <div className="pipeline">
      {order.map((node, i) => {
        const cls = i < idx ? "done" : i === idx ? "active" : "";
        return (
          <span key={node} style={{ display: "contents" }}>
            <span className={`pnode ${cls}`}>{node}</span>
            {i < order.length - 1 && <span className="parrow">→</span>}
          </span>
        );
      })}
    </div>
  );
}

export function RebalancePanel({ analysis, plan, job, onPreview, onExecute }: Props) {
  const [busy, setBusy] = useState<null | "preview" | "execute">(null);
  const [err, setErr] = useState<string | null>(null);

  const jobActive = job !== null && job.phase !== "completed";
  const jobDone = job !== null && job.phase === "completed";

  const run = async (which: "preview" | "execute", fn: () => Promise<void>) => {
    setErr(null);
    setBusy(which);
    try { await fn(); } catch (e) { setErr((e as Error).message); } finally { setBusy(null); }
  };

  return (
    <div className="stack">
      {/* Step 1 — preview */}
      {!jobActive && (
        <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
          <button className="ghost" disabled={busy !== null} onClick={() => run("preview", onPreview)}>
            {busy === "preview" ? "Previewing…" : "1 · Preview rebalance"}
          </button>
          <button
            className="primary"
            disabled={busy !== null || !plan || plan.orders.length === 0}
            onClick={() => run("execute", onExecute)}
          >
            {busy === "execute" ? "Submitting…" : "2 · Execute batch"}
          </button>
        </div>
      )}

      {err && <div className="negative mono" style={{ fontSize: 12.5 }}>{err}</div>}

      {/* Preview result */}
      {plan && !jobActive && !jobDone && (
        plan.orders.length === 0 ? (
          <div className="muted" style={{ fontSize: 13 }}>
            Portfolio is within ±{analysis.thresholdBps} bps on every asset — nothing to rebalance.
          </div>
        ) : (
          <div>
            <div className="muted" style={{ fontSize: 11, marginBottom: 6 }}>
              Planned batch — {plan.orders.length} order(s), est. notional {plan.estimatedNotional.toFixed(2)} {analysis.quoteCurrency}
            </div>
            <div style={{ overflowX: "auto" }}>
              <table className="table">
                <thead>
                  <tr><th>Side</th><th>Market</th><th>Qty</th><th>Price</th><th>Notional</th><th style={{ textAlign: "right" }}>Venue</th></tr>
                </thead>
                <tbody>
                  {plan.orders.map((o, i) => (
                    <tr key={i}>
                      <td><span className={`pill ${o.side}`}>{o.side}</span></td>
                      <td>{o.market}</td>
                      <td>{o.qty}</td>
                      <td>{o.price}</td>
                      <td>{o.notional.toFixed(2)}</td>
                      <td style={{ textAlign: "right" }}><span className="venue-tag">{o.venueId}</span></td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        )
      )}

      {/* Step 3 — monitor */}
      {job && (
        <div>
          <Pipeline phase={job.phase} />
          <div className={`progress ${job.phase === "completed" ? "done" : ""}`}>
            <span style={{ width: `${job.progressPct}%` }} />
          </div>
          <div className="row" style={{ justifyContent: "space-between", fontSize: 11.5, marginBottom: 8 }}>
            <span className="mono muted">{job.id}</span>
            <span className="mono muted">{job.mode} · {job.progressPct}%</span>
          </div>
          <div className="steps">
            {job.steps.map((s, i) => (
              <div key={i} className={`step ${s.status}`}>
                <span className="dot2" />
                <span className="step-main">
                  <span className="mono">{s.transfer.from} → {s.transfer.to}</span>
                  <span className="venue-tag" style={{ marginLeft: 8 }}>{s.transfer.venueId}</span>
                  <div className="step-note">{s.transfer.note}</div>
                </span>
                <span className="step-hash">{s.status === "pending" ? "—" : shortHash(s.transfer.txHash)}</span>
              </div>
            ))}
          </div>
          {jobDone && (
            <div className="row" style={{ marginTop: 12, gap: 8 }}>
              <span className="pill in">completed</span>
              <span className="muted" style={{ fontSize: 12 }}>portfolio aligned to targets — preview again to run another batch.</span>
              <button className="ghost" style={{ marginLeft: "auto" }} disabled={busy !== null} onClick={() => run("preview", onPreview)}>
                Preview again
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
