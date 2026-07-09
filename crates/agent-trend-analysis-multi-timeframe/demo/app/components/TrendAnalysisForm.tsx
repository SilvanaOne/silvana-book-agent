"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  short: string;
  mid: string;
  long: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  short: "5",
  mid: "20",
  long: "60",
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
        <div><label>Short SMA window</label><input type="number" step="1" value={v.short} onChange={(e) => upd("short", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Mid SMA window</label><input type="number" step="1" value={v.mid} onChange={(e) => upd("mid", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Long SMA window (buffer size)</label><input type="number" step="1" value={v.long} onChange={(e) => upd("long", e.target.value)} disabled={disabled || busy} /></div>
        <div style={{ fontSize: 12, color: "var(--text-faint)", alignSelf: "end" }}>Requires <span className="mono">short &lt; mid &lt; long</span>.</div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Multi-Timeframe"}</button>
    </form>
  );
}
