"use client";

import { useState } from "react";

export type FormValues = {
  historySize: string;
  kinds: string;
  markets: string;
  redactFields: string;
  parties: string;
  marketPool: string;
};

const DEFAULTS: FormValues = {
  historySize: "60",
  kinds: "settlement.settled,order.filled",
  markets: "CC-USDC",
  redactFields: "buyer,seller",
  parties: "party::alice,party::bob,party::charlie",
  marketPool: "CC-USDC,BTC-USD,ETH-USDC",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function DiscloseForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>History size (records)</label><input type="number" min={10} max={200} value={v.historySize} onChange={(e) => upd("historySize", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Keep kinds (comma; empty = all)</label><input value={v.kinds} onChange={(e) => upd("kinds", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Keep markets (empty = all)</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Redact payload fields</label><input value={v.redactFields} onChange={(e) => upd("redactFields", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Party pool (input flow generator)</label><input value={v.parties} onChange={(e) => upd("parties", e.target.value)} disabled={disabled || busy} /></div>
      <div><label>Market pool (input flow generator)</label><input value={v.marketPool} onChange={(e) => upd("marketPool", e.target.value)} disabled={disabled || busy} /></div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Loading…" : "Load history + filter"}</button>
    </form>
  );
}
