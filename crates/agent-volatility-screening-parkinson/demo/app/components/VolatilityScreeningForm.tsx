"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  window: string;
  periodSecs: string;
  periodsPerYear: string;
  pollSecs: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  window: "20",
  periodSecs: "10",
  periodsPerYear: "0", // 0 => auto (minutes/year scaled to periodSecs)
  pollSecs: "1",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function VolatilityScreeningForm({ disabled, onStart }: Props) {
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
        <div><label>Window (bars)</label><input type="number" step="1" value={v.window} onChange={(e) => upd("window", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Period (secs)</label><input type="number" step="1" value={v.periodSecs} onChange={(e) => upd("periodSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Poll (secs)</label><input type="number" step="1" value={v.pollSecs} onChange={(e) => upd("pollSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div>
          <label>Periods / year (0 = auto)</label>
          <input type="number" step="1" value={v.periodsPerYear} onChange={(e) => upd("periodsPerYear", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>
      <div style={{ fontSize: 12, color: "var(--text-faint)" }}>
        auto periods/year = 31 536 000 / periodSecs (minute bars → 525 600)
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Volatility"}</button>
    </form>
  );
}
