"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  window: string;
  rsiPeriod: string;
  bollingerK: string;
  macdFast: string;
  macdSlow: string;
  macdSignal: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  window: "30",
  rsiPeriod: "14",
  bollingerK: "2.0",
  macdFast: "12",
  macdSlow: "26",
  macdSignal: "9",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function TrendAnalysisForm({ disabled, onStart }: Props) {
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
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Window (SMA / EMA / BB)</label><input type="number" step="1" value={v.window} onChange={(e) => upd("window", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>RSI period</label><input type="number" step="1" value={v.rsiPeriod} onChange={(e) => upd("rsiPeriod", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Bollinger K (std devs)</label><input type="number" step="any" value={v.bollingerK} onChange={(e) => upd("bollingerK", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>MACD signal</label><input type="number" step="1" value={v.macdSignal} onChange={(e) => upd("macdSignal", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>MACD fast</label><input type="number" step="1" value={v.macdFast} onChange={(e) => upd("macdFast", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>MACD slow</label><input type="number" step="1" value={v.macdSlow} onChange={(e) => upd("macdSlow", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Trend Analysis"}</button>
    </form>
  );
}
