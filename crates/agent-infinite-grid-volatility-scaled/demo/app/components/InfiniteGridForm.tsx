"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  levels: string;
  quantityPerLevel: string;
  volWindow: string;
  stepMultiplier: string;
  minStepPct: string;
  maxStepPct: string;
  refreshSecs: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  levels: "5",
  quantityPerLevel: "1",
  volWindow: "40",
  stepMultiplier: "2.0",
  minStepPct: "0.1",
  maxStepPct: "5.0",
  refreshSecs: "10",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function InfiniteGridForm({ disabled, onStart }: Props) {
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
        <div><label>Qty / level</label><input type="number" step="any" value={v.quantityPerLevel} onChange={(e) => upd("quantityPerLevel", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Levels / side</label><input type="number" step="1" value={v.levels} onChange={(e) => upd("levels", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Refresh (s)</label><input type="number" step="1" value={v.refreshSecs} onChange={(e) => upd("refreshSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Vol window (samples)</label><input type="number" step="1" value={v.volWindow} onChange={(e) => upd("volWindow", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Step multiplier (×σ)</label><input type="number" step="any" value={v.stepMultiplier} onChange={(e) => upd("stepMultiplier", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Min step (%)</label><input type="number" step="any" value={v.minStepPct} onChange={(e) => upd("minStepPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max step (%)</label><input type="number" step="any" value={v.maxStepPct} onChange={(e) => upd("maxStepPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Volatility-Scaled Grid"}</button>
    </form>
  );
}
