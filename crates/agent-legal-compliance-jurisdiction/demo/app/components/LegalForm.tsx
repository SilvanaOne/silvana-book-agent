"use client";

import { useState } from "react";

export type FormValues = { policy: string; parties: string; markets: string; eventRatePerSec: string };

const DEFAULT_POLICY = `jurisdiction=US allowed=CC-USDC,BTC-USD max_notional=1000000 blocks=IR,KP
jurisdiction=EU prohibited=SCAM-USDC max_notional=500000
jurisdiction=SG allowed=CC-USDC,BTC-USD,SCAM-USDC max_notional=250000
jurisdiction=IR prohibited=BTC-USD,CC-USDC

party=party::alice jx=US
party=party::bob jx=EU
party=party::charlie jx=SG
party=party::dodgy jx=IR`;

const DEFAULTS: FormValues = {
  policy: DEFAULT_POLICY,
  parties: "party::alice,party::bob,party::charlie,party::dodgy",
  markets: "CC-USDC,BTC-USD,SCAM-USDC",
  eventRatePerSec: "3",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function LegalForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div>
        <label>Policy (jurisdiction=… and party=… lines)</label>
        <textarea value={v.policy} onChange={(e) => upd("policy", e.target.value)} disabled={disabled || busy} rows={10}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }} />
      </div>
      <div><label>Party pool</label><input value={v.parties} onChange={(e) => upd("parties", e.target.value)} disabled={disabled || busy} /></div>
      <div className="grid-2">
        <div><label>Market pool</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Event rate / sec</label><input type="number" step="any" min={0.1} max={50} value={v.eventRatePerSec} onChange={(e) => upd("eventRatePerSec", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start evaluating"}</button>
    </form>
  );
}
