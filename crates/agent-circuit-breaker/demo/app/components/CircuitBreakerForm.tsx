"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  maxDeviationPct: string;
  windowSecs: string;
  pauseSecs: string;
  startingPrice: string;
  startingOpenOrders: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  maxDeviationPct: "2.5",
  windowSecs: "30",
  pauseSecs: "60",
  startingPrice: "0.15",
  startingOpenOrders: "10",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function CircuitBreakerForm({ disabled, onStart }: Props) {
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
        <div><label>Max deviation (%)</label><input type="number" step="any" value={v.maxDeviationPct} onChange={(e) => upd("maxDeviationPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Window (secs)</label><input type="number" step="1" value={v.windowSecs} onChange={(e) => upd("windowSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Pause (secs)</label><input type="number" step="1" value={v.pauseSecs} onChange={(e) => upd("pauseSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting open orders</label><input type="number" step="1" value={v.startingOpenOrders} onChange={(e) => upd("startingOpenOrders", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Circuit Breaker"}</button>
    </form>
  );
}
