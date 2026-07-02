"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  instrument: string;
  target: string;
  tolerance: string;
  chunkSize: string;
  checkIntervalSecs: string;
  priceOffsetPct: string;
  startingBalance: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  instrument: "Amulet",
  target: "100",
  tolerance: "20",
  chunkSize: "5",
  checkIntervalSecs: "5",
  priceOffsetPct: "0",
  startingBalance: "60",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function InventoryMgmtForm({ disabled, onStart }: Props) {
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
        <div><label>Instrument</label><input value={v.instrument} onChange={(e) => upd("instrument", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Target balance</label><input type="number" step="any" value={v.target} onChange={(e) => upd("target", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Tolerance (±)</label><input type="number" step="any" value={v.tolerance} onChange={(e) => upd("tolerance", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Chunk size / order</label><input type="number" step="any" value={v.chunkSize} onChange={(e) => upd("chunkSize", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Check interval (s)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Price offset (%)</label><input type="number" step="any" value={v.priceOffsetPct} onChange={(e) => upd("priceOffsetPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting balance</label><input type="number" step="any" value={v.startingBalance} onChange={(e) => upd("startingBalance", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Inventory Mgmt"}</button>
    </form>
  );
}
