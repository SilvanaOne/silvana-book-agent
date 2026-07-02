"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  fastWindow: string;
  slowWindow: string;
  quantity: string;
  warmupSamples: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  fastWindow: "9",
  slowWindow: "21",
  quantity: "1",
  warmupSamples: "25",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function TrendFollowingForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setErr(null);
    const f = Number(v.fastWindow);
    const s = Number(v.slowWindow);
    if (!Number.isFinite(f) || !Number.isFinite(s) || f < 2 || s < 2) {
      setErr("fast and slow windows must both be >= 2");
      return;
    }
    if (f >= s) {
      setErr("fast window must be strictly less than slow window");
      return;
    }
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
        <div><label>Fast EMA window</label><input type="number" step="1" value={v.fastWindow} onChange={(e) => upd("fastWindow", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Slow EMA window</label><input type="number" step="1" value={v.slowWindow} onChange={(e) => upd("slowWindow", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Warmup samples</label><input type="number" step="1" value={v.warmupSamples} onChange={(e) => upd("warmupSamples", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Trend Following"}</button>
    </form>
  );
}
