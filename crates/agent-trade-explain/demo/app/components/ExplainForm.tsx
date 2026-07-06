"use client";

import { useState } from "react";

export type FormValues = { historySize: string; markets: string; includeOrders: boolean; includeSettlements: boolean; model: string };
const DEFAULTS: FormValues = {
  historySize: "40",
  markets: "CC-USDC,BTC-USD,ETH-USDC",
  includeOrders: true,
  includeSettlements: true,
  model: "silvana-mock-explain-v1",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ExplainForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>History size</label><input type="number" min={10} max={200} value={v.historySize} onChange={(e) => upd("historySize", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Model name</label><input value={v.model} onChange={(e) => upd("model", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Market pool</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
      <label className="row" style={{ gap: 8, alignItems: "center", fontSize: 13.5 }}>
        <input type="checkbox" checked={v.includeOrders} onChange={(e) => upd("includeOrders", e.target.checked)} disabled={disabled || busy} />
        <span>Explain <span className="mono">order.created</span> events</span>
      </label>
      <label className="row" style={{ gap: 8, alignItems: "center", fontSize: 13.5 }}>
        <input type="checkbox" checked={v.includeSettlements} onChange={(e) => upd("includeSettlements", e.target.checked)} disabled={disabled || busy} />
        <span>Explain <span className="mono">settlement.settled</span> events</span>
      </label>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Loading…" : "Load + explain"}</button>
    </form>
  );
}
