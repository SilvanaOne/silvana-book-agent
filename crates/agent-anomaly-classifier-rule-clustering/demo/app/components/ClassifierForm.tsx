"use client";

import { useState } from "react";

export type FormValues = { windowSecs: string; clusterThreshold: string; markets: string; ratePerSec: string; injectSpoofing: boolean; injectWashTrading: boolean };
const DEFAULTS: FormValues = { windowSecs: "60", clusterThreshold: "3", markets: "CC-USDC,BTC-USD,ETH-USDC", ratePerSec: "2", injectSpoofing: true, injectWashTrading: true };

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ClassifierForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>Window (seconds)</label><input type="number" min={1} value={v.windowSecs} onChange={(e) => upd("windowSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Cluster threshold</label><input type="number" min={2} value={v.clusterThreshold} onChange={(e) => upd("clusterThreshold", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Market pool</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
      <div><label>Rate (anomalies / sec)</label><input type="number" step="any" min={0.1} max={20} value={v.ratePerSec} onChange={(e) => upd("ratePerSec", e.target.value)} disabled={disabled || busy} /></div>
      <label className="row" style={{ gap: 8, alignItems: "center", fontSize: 13.5 }}>
        <input type="checkbox" checked={v.injectSpoofing} onChange={(e) => upd("injectSpoofing", e.target.checked)} disabled={disabled || busy} />
        <span>Inject spoofing bursts on first market</span>
      </label>
      <label className="row" style={{ gap: 8, alignItems: "center", fontSize: 13.5 }}>
        <input type="checkbox" checked={v.injectWashTrading} onChange={(e) => upd("injectWashTrading", e.target.checked)} disabled={disabled || busy} />
        <span>Inject wash-trading pattern on last market</span>
      </label>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start classifying"}</button>
    </form>
  );
}
