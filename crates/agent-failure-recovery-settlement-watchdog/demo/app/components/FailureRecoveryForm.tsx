"use client";

import { useState } from "react";

export type FormValues = {
  maxPendingAgeSecs: string;
  checkIntervalSecs: string;
  cancelRelatedOrders: boolean;
  dryRun: boolean;
  proposalArrivalPerTick: string;
  pendingStuckProbability: string;
  failureProbability: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  maxPendingAgeSecs: "60",
  checkIntervalSecs: "5",
  cancelRelatedOrders: true,
  dryRun: false,
  proposalArrivalPerTick: "0.4",
  pendingStuckProbability: "0.1",
  failureProbability: "0.05",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function FailureRecoveryForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setErr(null);
    setBusy(true);
    try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); }
  }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>Max pending age (s)</label><input type="number" step="1" value={v.maxPendingAgeSecs} onChange={(e) => upd("maxPendingAgeSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Check interval (s)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div style={{ display: "flex", alignItems: "center", gap: 8, paddingTop: 22 }}>
          <input id="fr-cancel" type="checkbox" checked={v.cancelRelatedOrders} onChange={(e) => upd("cancelRelatedOrders", e.target.checked)} disabled={disabled || busy} />
          <label htmlFor="fr-cancel" style={{ margin: 0 }}>Cancel related orders</label>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, paddingTop: 22 }}>
          <input id="fr-dryrun" type="checkbox" checked={v.dryRun} onChange={(e) => upd("dryRun", e.target.checked)} disabled={disabled || busy} />
          <label htmlFor="fr-dryrun" style={{ margin: 0 }}>Dry-run</label>
        </div>
      </div>
      <div className="grid-2">
        <div><label>Proposal arrival / tick</label><input type="number" step="any" value={v.proposalArrivalPerTick} onChange={(e) => upd("proposalArrivalPerTick", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Pending-stuck probability</label><input type="number" step="any" value={v.pendingStuckProbability} onChange={(e) => upd("pendingStuckProbability", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Failure probability</label><input type="number" step="any" value={v.failureProbability} onChange={(e) => upd("failureProbability", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Failure Recovery"}</button>
    </form>
  );
}
