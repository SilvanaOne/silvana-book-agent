"use client";

import { useState } from "react";

export type FormValues = {
  signalsFilePath: string;
  fromEnd: boolean;
  dryRun: boolean;
  signalArrivalPerTick: string;
  rejectionRate: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  signalsFilePath: "signals.jsonl",
  fromEnd: false,
  dryRun: false,
  signalArrivalPerTick: "0.4",
  rejectionRate: "0.05",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function SignalBotForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) {
    setV((p) => ({ ...p, [k]: val }));
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setErr(null);
    setBusy(true);
    try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); }
  }

  return (
    <form onSubmit={submit} className="stack">
      <div>
        <label>Signals file (mock path)</label>
        <input value={v.signalsFilePath} onChange={(e) => upd("signalsFilePath", e.target.value)} disabled={disabled || busy} />
      </div>
      <div className="grid-2">
        <div>
          <label>Signal arrival / tick (λ)</label>
          <input type="number" step="any" value={v.signalArrivalPerTick} onChange={(e) => upd("signalArrivalPerTick", e.target.value)} disabled={disabled || busy} />
        </div>
        <div>
          <label>Rejection rate (0..1)</label>
          <input type="number" step="any" value={v.rejectionRate} onChange={(e) => upd("rejectionRate", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Starting price (mid seed)</label>
          <input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} />
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 6, alignSelf: "end" }}>
          <label style={{ display: "flex", gap: 8, alignItems: "center", cursor: "pointer" }}>
            <input type="checkbox" checked={v.fromEnd} onChange={(e) => upd("fromEnd", e.target.checked)} disabled={disabled || busy} />
            <span>From end (skip history)</span>
          </label>
          <label style={{ display: "flex", gap: 8, alignItems: "center", cursor: "pointer" }}>
            <input type="checkbox" checked={v.dryRun} onChange={(e) => upd("dryRun", e.target.checked)} disabled={disabled || busy} />
            <span>Dry-run (no fills)</span>
          </label>
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Signal Bot"}</button>
    </form>
  );
}
