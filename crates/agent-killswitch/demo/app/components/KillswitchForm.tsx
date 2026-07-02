"use client";

import { useState } from "react";

export type FormValues = {
  maxOpenOrders: string;
  maxFailedSettlements: string;
  checkIntervalSecs: string;
  orderGrowthPerTick: string;
  failureRatePerTick: string;
  startingOpenOrders: string;
  startingFailed: string;
};

const DEFAULTS: FormValues = {
  maxOpenOrders: "200",
  maxFailedSettlements: "3",
  checkIntervalSecs: "3",
  orderGrowthPerTick: "2",
  failureRatePerTick: "0.05",
  startingOpenOrders: "100",
  startingFailed: "0",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function KillswitchForm({ disabled, onStart }: Props) {
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
        <div><label>Max open orders</label><input type="number" step="1" value={v.maxOpenOrders} onChange={(e) => upd("maxOpenOrders", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max failed settlements</label><input type="number" step="1" value={v.maxFailedSettlements} onChange={(e) => upd("maxFailedSettlements", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Check interval (secs)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Order growth / tick</label><input type="number" step="any" value={v.orderGrowthPerTick} onChange={(e) => upd("orderGrowthPerTick", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Failure rate / tick (0-1)</label><input type="number" step="any" value={v.failureRatePerTick} onChange={(e) => upd("failureRatePerTick", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting open orders</label><input type="number" step="1" value={v.startingOpenOrders} onChange={(e) => upd("startingOpenOrders", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting failed</label><input type="number" step="1" value={v.startingFailed} onChange={(e) => upd("startingFailed", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Arming…" : "Arm Killswitch"}</button>
    </form>
  );
}
