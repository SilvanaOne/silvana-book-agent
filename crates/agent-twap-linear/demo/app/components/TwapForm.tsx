"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  side: "buy" | "sell";
  total: string;
  slices: string;
  durationSecs: string;
  priceOffsetPct: string;
  limitPrice: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  side: "buy",
  total: "100",
  slices: "10",
  durationSecs: "60",
  priceOffsetPct: "0",
  limitPrice: "",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function TwapForm({ disabled, onStart }: Props) {
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
        <div>
          <label>Side</label>
          <select value={v.side} onChange={(e) => upd("side", e.target.value as "buy" | "sell")} disabled={disabled || busy}>
            <option value="buy">buy</option>
            <option value="sell">sell</option>
          </select>
        </div>
      </div>
      <div className="grid-2">
        <div><label>Total quantity</label><input type="number" step="any" value={v.total} onChange={(e) => upd("total", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Slices</label><input type="number" step="1" value={v.slices} onChange={(e) => upd("slices", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Duration (secs)</label><input type="number" step="1" value={v.durationSecs} onChange={(e) => upd("durationSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Price offset (%)</label><input type="number" step="any" value={v.priceOffsetPct} onChange={(e) => upd("priceOffsetPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Limit price (optional)</label><input type="number" step="any" value={v.limitPrice} placeholder="none" onChange={(e) => upd("limitPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start TWAP"}</button>
    </form>
  );
}
