"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  orderArrivalPerTick: string;
  autoApprovalEnabled: boolean;
  autoApprovalThreshold: string;
  reviewerName: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  orderArrivalPerTick: "0.4",
  autoApprovalEnabled: false,
  autoApprovalThreshold: "1000",
  reviewerName: "operator",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function HumanApprovalForm({ disabled, onStart }: Props) {
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
        <div><label>Market</label><input value={v.market} onChange={(e) => upd("market", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Reviewer name</label><input value={v.reviewerName} onChange={(e) => upd("reviewerName", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Order arrival / tick</label><input type="number" step="any" min="0" max="10" value={v.orderArrivalPerTick} onChange={(e) => upd("orderArrivalPerTick", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div>
          <label>Auto-approval</label>
          <label className="row" style={{ gap: 6, alignItems: "center", marginTop: 4 }}>
            <input type="checkbox" checked={v.autoApprovalEnabled} onChange={(e) => upd("autoApprovalEnabled", e.target.checked)} disabled={disabled || busy} />
            <span className="mono" style={{ fontSize: 12 }}>enabled</span>
          </label>
        </div>
        <div><label>Auto-approval threshold (notional)</label><input type="number" step="any" min="0" value={v.autoApprovalThreshold} onChange={(e) => upd("autoApprovalThreshold", e.target.value)} disabled={disabled || busy || !v.autoApprovalEnabled} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Human Approval"}</button>
    </form>
  );
}
