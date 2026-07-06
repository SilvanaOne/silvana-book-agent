"use client";

import { useState } from "react";

export type FormValues = {
  policy: string;
  parties: string;
  markets: string;
  marketFilter: string;
  eventRatePerSec: string;
  emitAccepts: boolean;
};

const DEFAULT_POLICY = `blocked_pair=party::sanctioned,party::any
party_cap=party::whale window_hours=24 max_notional=50000
allowed=party::allowed_a
allowed=party::allowed_b
allowed=party::whale
blocked_market=SCAM-USDC`;

const DEFAULTS: FormValues = {
  policy: DEFAULT_POLICY,
  parties: "party::allowed_a,party::allowed_b,party::whale,party::sanctioned,party::random_c",
  markets: "CC-USDC,BTC-USD,SCAM-USDC",
  marketFilter: "",
  eventRatePerSec: "2",
  emitAccepts: false,
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ComplianceForm({ disabled, onStart }: Props) {
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
        <label>Policy (one rule per line)</label>
        <textarea
          value={v.policy}
          onChange={(e) => upd("policy", e.target.value)}
          disabled={disabled || busy}
          rows={8}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }}
        />
      </div>
      <div>
        <label>Party pool (comma-separated)</label>
        <input value={v.parties} onChange={(e) => upd("parties", e.target.value)} disabled={disabled || busy} />
      </div>
      <div className="grid-2">
        <div><label>Market pool (comma-separated)</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Filter market (empty = all)</label><input value={v.marketFilter} onChange={(e) => upd("marketFilter", e.target.value)} disabled={disabled || busy} placeholder="CC-USDC" /></div>
      </div>
      <div className="grid-2">
        <div><label>Event rate / sec</label><input type="number" step="any" min={0.1} max={50} value={v.eventRatePerSec} onChange={(e) => upd("eventRatePerSec", e.target.value)} disabled={disabled || busy} /></div>
        <div>
          <label>&nbsp;</label>
          <label className="row" style={{ gap: 8, alignItems: "center", paddingTop: 6, fontSize: 13.5 }}>
            <input type="checkbox" checked={v.emitAccepts} onChange={(e) => upd("emitAccepts", e.target.checked)} disabled={disabled || busy} />
            <span><span className="mono">--emit-accepts</span> (default: rejects only)</span>
          </label>
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start screening"}</button>
    </form>
  );
}
