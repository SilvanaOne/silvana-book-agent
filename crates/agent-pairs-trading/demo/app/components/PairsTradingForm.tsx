"use client";

import { useState } from "react";

export type FormValues = {
  marketA: string;
  marketB: string;
  targetRatio: string;
  thresholdPct: string;
  quantityA: string;
  quantityB: string;
  startingPriceA: string;
  startingPriceB: string;
};

const DEFAULTS: FormValues = {
  marketA: "CC-USDC",
  marketB: "CC-USDT",
  targetRatio: "1.0",
  thresholdPct: "1.0",
  quantityA: "10",
  quantityB: "10",
  startingPriceA: "0.15",
  startingPriceB: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function PairsTradingForm({ disabled, onStart }: Props) {
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
        <div><label>Market A</label><input value={v.marketA} onChange={(e) => upd("marketA", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Market B</label><input value={v.marketB} onChange={(e) => upd("marketB", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Target ratio (mid A / mid B)</label><input type="number" step="any" value={v.targetRatio} onChange={(e) => upd("targetRatio", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Threshold (%)</label><input type="number" step="any" value={v.thresholdPct} onChange={(e) => upd("thresholdPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Quantity A</label><input type="number" step="any" value={v.quantityA} onChange={(e) => upd("quantityA", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Quantity B</label><input type="number" step="any" value={v.quantityB} onChange={(e) => upd("quantityB", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price A</label><input type="number" step="any" value={v.startingPriceA} onChange={(e) => upd("startingPriceA", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price B</label><input type="number" step="any" value={v.startingPriceB} onChange={(e) => upd("startingPriceB", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Pairs Trading"}</button>
    </form>
  );
}
