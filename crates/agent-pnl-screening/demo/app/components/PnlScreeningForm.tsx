"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  snapshotIntervalSecs: string;
  tradeArrivalPerTick: string;
  avgTradeQty: string;
  startingPrice: string;
  startingPosition: string;
  startingCostBasis: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  snapshotIntervalSecs: "10",
  tradeArrivalPerTick: "0.3",
  avgTradeQty: "5",
  startingPrice: "0.15",
  startingPosition: "0",
  startingCostBasis: "0",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function PnlScreeningForm({ disabled, onStart }: Props) {
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
        <div><label>Snapshot interval (secs)</label><input type="number" step="1" value={v.snapshotIntervalSecs} onChange={(e) => upd("snapshotIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Trade arrival / tick</label><input type="number" step="any" value={v.tradeArrivalPerTick} onChange={(e) => upd("tradeArrivalPerTick", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Avg trade qty</label><input type="number" step="any" value={v.avgTradeQty} onChange={(e) => upd("avgTradeQty", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting position</label><input type="number" step="any" value={v.startingPosition} onChange={(e) => upd("startingPosition", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting cost basis</label><input type="number" step="any" value={v.startingCostBasis} onChange={(e) => upd("startingCostBasis", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start PnL Screening"}</button>
    </form>
  );
}
