"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  window: string;
  k: string;
  quantity: string;
  warmupSamples: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  window: "20",
  k: "2.0",
  quantity: "1",
  warmupSamples: "20",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function MrForm({ disabled, onStart }: Props) {
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
        <div><label>Quantity / entry</label><input type="number" step="any" value={v.quantity} onChange={(e) => upd("quantity", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Window (samples)</label><input type="number" step="1" value={v.window} onChange={(e) => upd("window", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>k (band multiplier)</label><input type="number" step="any" value={v.k} onChange={(e) => upd("k", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Warmup samples</label><input type="number" step="1" value={v.warmupSamples} onChange={(e) => upd("warmupSamples", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Bollinger Mean Reversion"}</button>
    </form>
  );
}
