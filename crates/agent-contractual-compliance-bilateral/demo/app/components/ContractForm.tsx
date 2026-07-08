"use client";

import { useState } from "react";

export type FormValues = {
  contracts: string;
  parties: string;
  markets: string;
  eventRatePerSec: string;
  statusIntervalSecs: string;
};

const DEFAULT_CONTRACTS = `id=lp-cc-usdc counterparty=party::whale market=CC-USDC window_hours=1 min=2000 max=20000
id=lp-btc-usd counterparty=party::whale market=BTC-USD window_hours=1 max=15000
id=trend-cc counterparty=party::market_maker market=CC-USDC window_hours=1 min=500 expires=2027-12-31`;

const DEFAULTS: FormValues = {
  contracts: DEFAULT_CONTRACTS,
  parties: "party::whale,party::market_maker,party::small_client",
  markets: "CC-USDC,BTC-USD",
  eventRatePerSec: "3",
  statusIntervalSecs: "6",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ContractForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }

  async function submit(e: React.FormEvent) {
    e.preventDefault(); setErr(null); setBusy(true);
    try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); }
  }

  return (
    <form onSubmit={submit} className="stack">
      <div>
        <label>Contracts (one per line, key=value tokens)</label>
        <textarea
          value={v.contracts}
          onChange={(e) => upd("contracts", e.target.value)}
          disabled={disabled || busy}
          rows={7}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }}
        />
      </div>
      <div>
        <label>Counterparty pool</label>
        <input value={v.parties} onChange={(e) => upd("parties", e.target.value)} disabled={disabled || busy} />
      </div>
      <div className="grid-2">
        <div><label>Market pool</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Event rate / sec</label><input type="number" step="any" min={0.1} max={50} value={v.eventRatePerSec} onChange={(e) => upd("eventRatePerSec", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div>
        <label>Status interval (seconds)</label>
        <input type="number" min={1} max={600} value={v.statusIntervalSecs} onChange={(e) => upd("statusIntervalSecs", e.target.value)} disabled={disabled || busy} />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start tracking"}</button>
    </form>
  );
}
