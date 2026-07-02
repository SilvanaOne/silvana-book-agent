"use client";

import { useState } from "react";

export type FormValues = {
  markets: string;
  wChange: string;
  wVolume: string;
  wSpread: string;
  pollSecs: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  markets: "CC-USDC,BTC-USD,CETH-CC",
  wChange: "1.0",
  wVolume: "0.5",
  wSpread: "2.0",
  pollSecs: "5",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function YieldRotationForm({ disabled, onStart }: Props) {
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
      <div>
        <label>Markets (comma-separated)</label>
        <input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} />
      </div>
      <div className="grid-2">
        <div><label>Weight change (w_change)</label><input type="number" step="any" value={v.wChange} onChange={(e) => upd("wChange", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Weight volume (w_volume)</label><input type="number" step="any" value={v.wVolume} onChange={(e) => upd("wVolume", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Weight spread (w_spread)</label><input type="number" step="any" value={v.wSpread} onChange={(e) => upd("wSpread", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Poll cadence (secs)</label><input type="number" step="1" value={v.pollSecs} onChange={(e) => upd("pollSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div>
        <label>Starting price (sim seed)</label>
        <input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Yield Rotation"}</button>
    </form>
  );
}
