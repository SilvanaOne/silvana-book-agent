"use client";

import { useState } from "react";

export type FormValues = {
  minCc: string;
  maxCc: string;
  sinkParty: string;
  checkIntervalSecs: string;
  incomeRate: string;
  startingBalance: string;
};

const DEFAULTS: FormValues = {
  minCc: "20",
  maxCc: "100",
  sinkParty: "party-treasury",
  checkIntervalSecs: "5",
  incomeRate: "0.5",
  startingBalance: "50",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function CashBufferForm({ disabled, onStart }: Props) {
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
        <div><label>Min CC (floor)</label><input type="number" step="any" value={v.minCc} onChange={(e) => upd("minCc", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max CC (ceiling)</label><input type="number" step="any" value={v.maxCc} onChange={(e) => upd("maxCc", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Sink party</label><input value={v.sinkParty} onChange={(e) => upd("sinkParty", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Check interval (secs)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Income rate / tick (CC)</label><input type="number" step="any" value={v.incomeRate} onChange={(e) => upd("incomeRate", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting balance (CC)</label><input type="number" step="any" value={v.startingBalance} onChange={(e) => upd("startingBalance", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Cash Buffer"}</button>
    </form>
  );
}
