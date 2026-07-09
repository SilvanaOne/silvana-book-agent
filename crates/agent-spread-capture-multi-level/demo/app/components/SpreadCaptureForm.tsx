"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  levels: string;
  innerSpreadBps: string;
  stepBps: string;
  quantityPerLevel: string;
  maxInventory: string;
  refreshSecs: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  levels: "3",
  innerSpreadBps: "25",
  stepBps: "25",
  quantityPerLevel: "1",
  maxInventory: "10",
  refreshSecs: "15",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function SpreadCaptureForm({ disabled, onStart }: Props) {
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
        <div><label>Levels per side</label><input type="number" step="1" min="1" value={v.levels} onChange={(e) => upd("levels", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Inner spread (bps, per side)</label><input type="number" step="1" value={v.innerSpreadBps} onChange={(e) => upd("innerSpreadBps", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Step between levels (bps)</label><input type="number" step="1" value={v.stepBps} onChange={(e) => upd("stepBps", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Quantity / level</label><input type="number" step="any" value={v.quantityPerLevel} onChange={(e) => upd("quantityPerLevel", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max inventory (base units)</label><input type="number" step="any" value={v.maxInventory} onChange={(e) => upd("maxInventory", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Refresh interval (secs)</label><input type="number" step="1" value={v.refreshSecs} onChange={(e) => upd("refreshSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Spread Capture (multi-level)"}</button>
    </form>
  );
}
