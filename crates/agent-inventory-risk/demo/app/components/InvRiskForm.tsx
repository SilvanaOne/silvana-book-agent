"use client";

import { useState } from "react";

export type FormValues = {
  instrument: string;
  hedgeMarket: string;
  target: string;
  softTolerance: string;
  hardTolerance: string;
  startingBalance: string;
  startingPrice: string;
  autoHedge: boolean;
  checkIntervalSecs: string;
  driftPerCycle: string;
};

const DEFAULTS: FormValues = {
  instrument: "Amulet",
  hedgeMarket: "CC-USDC",
  target: "100",
  softTolerance: "10",
  hardTolerance: "25",
  startingBalance: "100",
  startingPrice: "0.15",
  autoHedge: true,
  checkIntervalSecs: "5",
  driftPerCycle: "1.5",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function InvRiskForm({ disabled, onStart }: Props) {
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
        <div><label>Instrument</label><input value={v.instrument} onChange={(e) => upd("instrument", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Hedge market (BASE-QUOTE)</label><input value={v.hedgeMarket} onChange={(e) => upd("hedgeMarket", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Target balance</label><input type="number" step="any" value={v.target} onChange={(e) => upd("target", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting balance</label><input type="number" step="any" value={v.startingBalance} onChange={(e) => upd("startingBalance", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Soft tolerance (± signals)</label><input type="number" step="any" value={v.softTolerance} onChange={(e) => upd("softTolerance", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Hard tolerance (± auto-hedge)</label><input type="number" step="any" value={v.hardTolerance} onChange={(e) => upd("hardTolerance", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Check interval (seconds)</label><input type="number" min={1} max={300} value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Exposure drift / cycle (bias in units)</label><input type="number" step="any" value={v.driftPerCycle} onChange={(e) => upd("driftPerCycle", e.target.value)} disabled={disabled || busy} /></div>
        <div>
          <label>&nbsp;</label>
          <label className="row" style={{ gap: 8, alignItems: "center", fontSize: 13.5, paddingTop: 6 }}>
            <input type="checkbox" checked={v.autoHedge} onChange={(e) => upd("autoHedge", e.target.checked)} disabled={disabled || busy} />
            <span><span className="mono">--auto-hedge</span></span>
          </label>
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start monitoring"}</button>
    </form>
  );
}
