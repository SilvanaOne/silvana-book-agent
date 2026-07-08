"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  stepPct: string;
  levels: string;
  quantityPerLevel: string;
  refreshSecs: string;
  driftThresholdPct: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  stepPct: "0.5",
  levels: "5",
  quantityPerLevel: "1",
  refreshSecs: "10",
  driftThresholdPct: "0.3",
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
        <div><label>Step (%)</label><input type="number" step="any" value={v.stepPct} onChange={(e) => upd("stepPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Levels / side</label><input type="number" step="1" value={v.levels} onChange={(e) => upd("levels", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Refresh (s)</label><input type="number" step="1" value={v.refreshSecs} onChange={(e) => upd("refreshSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Drift threshold (%)</label><input type="number" step="any" value={v.driftThresholdPct} onChange={(e) => upd("driftThresholdPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Infinite Grid"}</button>
    </form>
  );
}
