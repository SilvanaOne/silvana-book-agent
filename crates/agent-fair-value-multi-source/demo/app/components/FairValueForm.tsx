"use client";

import { useState } from "react";
import type { AggMethod } from "@/lib/fairvalue-engine";

export type FormValues = {
  market: string;
  sources: string;      // CSV
  method: AggMethod;
  pollSecs: string;
  sourceNoisePct: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  sources: "binance_spot,bybit,coingecko",
  method: "median",
  pollSecs: "5",
  sourceNoisePct: "0.5",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function FairValueForm({ disabled, onStart }: Props) {
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
        <div>
          <label>Aggregation method</label>
          <select value={v.method} onChange={(e) => upd("method", e.target.value as AggMethod)} disabled={disabled || busy}>
            <option value="median">median</option>
            <option value="mean">mean</option>
            <option value="trimmed-mean">trimmed-mean</option>
          </select>
        </div>
      </div>
      <div>
        <label>Sources (comma-separated)</label>
        <input value={v.sources} onChange={(e) => upd("sources", e.target.value)} disabled={disabled || busy} placeholder="binance_spot,bybit,coingecko" />
      </div>
      <div className="grid-2">
        <div><label>Poll interval (s)</label><input type="number" step="1" value={v.pollSecs} onChange={(e) => upd("pollSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Source noise (%)</label><input type="number" step="any" value={v.sourceNoisePct} onChange={(e) => upd("sourceNoisePct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (true mid)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Fair Value"}</button>
    </form>
  );
}
