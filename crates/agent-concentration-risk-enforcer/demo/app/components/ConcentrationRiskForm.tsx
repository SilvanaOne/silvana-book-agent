"use client";

import { useState } from "react";
import { DEFAULT_INSTRUMENTS } from "@/lib/concentrationrisk-engine";

export type FormValues = {
  instrumentsJson: string;
  maxSharePct: string;
  minSharePct: string;
  checkIntervalSecs: string;
  dryRun: boolean;
  balanceDriftPerTick: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  instrumentsJson: JSON.stringify(DEFAULT_INSTRUMENTS, null, 2),
  maxSharePct: "60",
  minSharePct: "10",
  checkIntervalSecs: "5",
  dryRun: false,
  balanceDriftPerTick: "0.5",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ConcentrationRiskForm({ disabled, onStart }: Props) {
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
        <label>Instruments (JSON: name, market, balance, priceMultiplier, bidsActive, offersActive)</label>
        <textarea
          value={v.instrumentsJson}
          onChange={(e) => upd("instrumentsJson", e.target.value)}
          disabled={disabled || busy}
          rows={10}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12 }}
        />
      </div>
      <div className="grid-2">
        <div><label>Max share (%) — cancel BIDs above</label><input type="number" step="any" value={v.maxSharePct} onChange={(e) => upd("maxSharePct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Min share (%) — cancel OFFERs below</label><input type="number" step="any" value={v.minSharePct} onChange={(e) => upd("minSharePct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Check interval (secs)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Balance drift / tick</label><input type="number" step="any" value={v.balanceDriftPerTick} onChange={(e) => upd("balanceDriftPerTick", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div style={{ display: "flex", alignItems: "end", gap: 8 }}>
          <label style={{ display: "flex", alignItems: "center", gap: 8, cursor: "pointer" }}>
            <input type="checkbox" checked={v.dryRun} onChange={(e) => upd("dryRun", e.target.checked)} disabled={disabled || busy} />
            <span>Dry-run (log only, don&apos;t cancel)</span>
          </label>
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Concentration Risk"}</button>
    </form>
  );
}
