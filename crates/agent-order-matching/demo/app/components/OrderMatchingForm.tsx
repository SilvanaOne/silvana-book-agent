"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  buyTrigger: string;
  sellTrigger: string;
  quantity: string;
  bookSpreadBps: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  buyTrigger: "0.145",
  sellTrigger: "0.155",
  quantity: "5",
  bookSpreadBps: "10",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function OrderMatchingForm({ disabled, onStart }: Props) {
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
        <div><label>Quantity / snipe</label><input type="number" step="any" value={v.quantity} onChange={(e) => upd("quantity", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Buy trigger (best_offer ≤ this) — optional</label><input type="number" step="any" placeholder="e.g. 0.145" value={v.buyTrigger} onChange={(e) => upd("buyTrigger", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Sell trigger (best_bid ≥ this) — optional</label><input type="number" step="any" placeholder="e.g. 0.155" value={v.sellTrigger} onChange={(e) => upd("sellTrigger", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Book spread (bps)</label><input type="number" step="any" value={v.bookSpreadBps} onChange={(e) => upd("bookSpreadBps", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Order Matching"}</button>
    </form>
  );
}
