"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  maxDriftPct: string;
  checkIntervalSecs: string;
  orderArrivalPerTick: string;
  dryRun: boolean;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  maxDriftPct: "1.5",
  checkIntervalSecs: "5",
  orderArrivalPerTick: "0.3",
  dryRun: false,
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function OrderExpiryForm({ disabled, onStart }: Props) {
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
        <div><label>Max drift (%) vs mid</label><input type="number" step="any" value={v.maxDriftPct} onChange={(e) => upd("maxDriftPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Check interval (s)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Order arrival / tick (mock)</label><input type="number" step="any" value={v.orderArrivalPerTick} onChange={(e) => upd("orderArrivalPerTick", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div style={{ display: "flex", alignItems: "flex-end", gap: 8 }}>
          <label style={{ display: "inline-flex", alignItems: "center", gap: 8, fontSize: 13 }}>
            <input type="checkbox" checked={v.dryRun} onChange={(e) => upd("dryRun", e.target.checked)} disabled={disabled || busy} />
            Dry-run (log-only, no cancel)
          </label>
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Order Expiry"}</button>
    </form>
  );
}
