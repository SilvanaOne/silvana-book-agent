"use client";

import { useState } from "react";
import type { OracleSource } from "@/lib/oracle-engine";

export type FormValues = {
  marketsCsv: string;
  source: OracleSource;
  pollSecs: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  marketsCsv: "CC-USDC,BTC-USD",
  source: "binance_spot",
  pollSecs: "5",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function OracleForm({ disabled, onStart }: Props) {
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
        <input value={v.marketsCsv} onChange={(e) => upd("marketsCsv", e.target.value)} disabled={disabled || busy} />
      </div>
      <div className="grid-2">
        <div>
          <label>Source</label>
          <select value={v.source} onChange={(e) => upd("source", e.target.value as OracleSource)} disabled={disabled || busy}>
            <option value="binance_spot">binance_spot</option>
            <option value="bybit">bybit</option>
            <option value="coingecko">coingecko</option>
          </select>
        </div>
        <div>
          <label>Poll interval (secs)</label>
          <input type="number" step="1" min="1" value={v.pollSecs} onChange={(e) => upd("pollSecs", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>
      <div>
        <label>Starting price (applied to all markets)</label>
        <input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Oracle"}</button>
    </form>
  );
}
