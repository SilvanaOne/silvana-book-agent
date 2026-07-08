"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  side: "buy" | "sell";
  amountPerOrder: string;
  intervalSecs: string;
  priceOffsetPct: string;
  maxTotal: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  side: "buy",
  amountPerOrder: "10",
  intervalSecs: "5",
  priceOffsetPct: "-0.3",
  maxTotal: "100",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function DcaForm({ disabled, onStart }: Props) {
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
        <div><label>Side</label><select value={v.side} onChange={(e) => upd("side", e.target.value as "buy" | "sell")} disabled={disabled || busy}>
          <option value="buy">BUY (accumulate base)</option>
          <option value="sell">SELL (distribute base)</option>
        </select></div>
      </div>
      <div className="grid-2">
        <div><label>Amount per order</label><input type="number" step="any" value={v.amountPerOrder} onChange={(e) => upd("amountPerOrder", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Interval (sec)</label><input type="number" step="any" value={v.intervalSecs} onChange={(e) => upd("intervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Price offset (%)</label><input type="number" step="any" value={v.priceOffsetPct} onChange={(e) => upd("priceOffsetPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max total (optional)</label><input type="number" step="any" placeholder="cap" value={v.maxTotal} onChange={(e) => upd("maxTotal", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start DCA"}</button>
    </form>
  );
}
