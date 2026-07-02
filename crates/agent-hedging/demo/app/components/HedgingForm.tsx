"use client";

import { useState } from "react";

export type FormValues = {
  exposureInstrument: string;
  hedgeMarket: string;
  targetBalance: string;
  tolerance: string;
  hedgeFraction: string;
  checkIntervalSecs: string;
  exposureDriftPerTick: string;
  startingBalance: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  exposureInstrument: "Amulet",
  hedgeMarket: "CC-USDC",
  targetBalance: "50",
  tolerance: "5",
  hedgeFraction: "0.5",
  checkIntervalSecs: "5",
  exposureDriftPerTick: "0.5",
  startingBalance: "60",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function HedgingForm({ disabled, onStart }: Props) {
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
        <div><label>Exposure instrument</label><input value={v.exposureInstrument} onChange={(e) => upd("exposureInstrument", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Hedge market</label><input value={v.hedgeMarket} onChange={(e) => upd("hedgeMarket", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Target balance</label><input type="number" step="any" value={v.targetBalance} onChange={(e) => upd("targetBalance", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Tolerance (±)</label><input type="number" step="any" value={v.tolerance} onChange={(e) => upd("tolerance", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Hedge fraction (0, 1]</label><input type="number" step="any" value={v.hedgeFraction} onChange={(e) => upd("hedgeFraction", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Check interval (s)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Exposure drift / tick (σ)</label><input type="number" step="any" value={v.exposureDriftPerTick} onChange={(e) => upd("exposureDriftPerTick", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting balance</label><input type="number" step="any" value={v.startingBalance} onChange={(e) => upd("startingBalance", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Hedging"}</button>
    </form>
  );
}
