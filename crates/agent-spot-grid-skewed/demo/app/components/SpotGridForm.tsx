"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  midPrice: string;
  bidLevels: string;
  offerLevels: string;
  stepPct: string;
  baseQuantity: string;
  startingBalance: string;
  targetBalance: string;
  alpha: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  midPrice: "0.15",
  bidLevels: "5",
  offerLevels: "5",
  stepPct: "1.0",
  baseQuantity: "1",
  startingBalance: "100",
  targetBalance: "100",
  alpha: "0.6",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function SpotGridForm({ disabled, onStart }: Props) {
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
        <div><label>Mid price (grid center)</label><input type="number" step="any" value={v.midPrice} onChange={(e) => upd("midPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Bid levels</label><input type="number" step="1" value={v.bidLevels} onChange={(e) => upd("bidLevels", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Offer levels</label><input type="number" step="1" value={v.offerLevels} onChange={(e) => upd("offerLevels", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Step % (between rungs)</label><input type="number" step="any" value={v.stepPct} onChange={(e) => upd("stepPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Base qty / level</label><input type="number" step="any" value={v.baseQuantity} onChange={(e) => upd("baseQuantity", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting balance (base instrument)</label><input type="number" step="any" value={v.startingBalance} onChange={(e) => upd("startingBalance", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Target balance</label><input type="number" step="any" value={v.targetBalance} onChange={(e) => upd("targetBalance", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Alpha (skew intensity 0..1)</label><input type="number" step="0.05" min="0" max="1" value={v.alpha} onChange={(e) => upd("alpha", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Skewed Grid"}</button>
    </form>
  );
}
