"use client";

import { useState } from "react";

export type FormValues = {
  markets: string;              // CSV: "CC-USDC,BTC-USD"
  instruments: string;          // CSV of name:balance pairs
  snapshotIntervalSecs: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  markets: "CC-USDC,BTC-USD",
  instruments: "Amulet:50,CBTC:0.5,CETH:5",
  snapshotIntervalSecs: "10",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function PortfolioHealthForm({ disabled, onStart }: Props) {
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
        <label>Markets (CSV)</label>
        <input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} placeholder="CC-USDC,BTC-USD" />
      </div>
      <div>
        <label>Instruments (name:balance, comma-separated)</label>
        <input value={v.instruments} onChange={(e) => upd("instruments", e.target.value)} disabled={disabled || busy} placeholder="Amulet:50,CBTC:0.5,CETH:5" />
      </div>
      <div className="grid-2">
        <div><label>Snapshot interval (s)</label><input type="number" step="1" value={v.snapshotIntervalSecs} onChange={(e) => upd("snapshotIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Portfolio Health"}</button>
    </form>
  );
}
