"use client";

import { useState } from "react";

export type FormValues = {
  historyDays: string;
  recordsPerDay: string;
  markets: string;
  settlementWindowSecs: string;
  rapidCancelWindowMs: string;
  layerThreshold: string;
  layerBandPct: string;
  burstCount: string;
  burstWindowSecs: string;
  injectAnomalies: boolean;
};

const DEFAULTS: FormValues = {
  historyDays: "3",
  recordsPerDay: "80",
  markets: "CC-USDC,BTC-USD,SCAM-USDC",
  settlementWindowSecs: "600",
  rapidCancelWindowMs: "1500",
  layerThreshold: "5",
  layerBandPct: "1.5",
  burstCount: "4",
  burstWindowSecs: "8",
  injectAnomalies: true,
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function AnomalyForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>History days</label><input type="number" min={1} max={14} value={v.historyDays} onChange={(e) => upd("historyDays", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Records / day</label><input type="number" min={20} max={500} value={v.recordsPerDay} onChange={(e) => upd("recordsPerDay", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Market pool</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
      <div className="grid-2">
        <div><label>Stuck settlement window (s)</label><input type="number" min={1} value={v.settlementWindowSecs} onChange={(e) => upd("settlementWindowSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Rapid cancel window (ms)</label><input type="number" min={1} value={v.rapidCancelWindowMs} onChange={(e) => upd("rapidCancelWindowMs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Layer threshold</label><input type="number" min={2} value={v.layerThreshold} onChange={(e) => upd("layerThreshold", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Layer band (%)</label><input type="number" step="any" min={0.01} value={v.layerBandPct} onChange={(e) => upd("layerBandPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Burst cancels</label><input type="number" min={2} value={v.burstCount} onChange={(e) => upd("burstCount", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Burst window (s)</label><input type="number" min={1} value={v.burstWindowSecs} onChange={(e) => upd("burstWindowSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <label className="row" style={{ gap: 8, alignItems: "center", fontSize: 13.5 }}>
        <input type="checkbox" checked={v.injectAnomalies} onChange={(e) => upd("injectAnomalies", e.target.checked)} disabled={disabled || busy} />
        <span>Inject synthetic anomalies (rapid cancels + layer bursts)</span>
      </label>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Scanning…" : "Load + scan"}</button>
    </form>
  );
}
