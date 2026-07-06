"use client";

import { useState } from "react";

export type FormValues = {
  partyId: string;
  endpoint: string;
  market: string;
  failureRate: string;
  latencyMultiplier: string;
  runIntervalSecs: string;
};

const DEFAULTS: FormValues = {
  partyId: "party::alice",
  endpoint: "https://devnet.silvana.one:443",
  market: "CC-USDC",
  failureRate: "0.05",
  latencyMultiplier: "1.0",
  runIntervalSecs: "20",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function TestRunForm({ disabled, onStart }: Props) {
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
        <label>Party ID</label>
        <input value={v.partyId} onChange={(e) => upd("partyId", e.target.value)} disabled={disabled || busy} />
      </div>
      <div>
        <label>gRPC endpoint</label>
        <input value={v.endpoint} onChange={(e) => upd("endpoint", e.target.value)} disabled={disabled || busy} />
      </div>
      <div className="grid-2">
        <div><label>Market (optional — enables get_price + depth)</label><input value={v.market} onChange={(e) => upd("market", e.target.value)} disabled={disabled || busy} placeholder="CC-USDC or blank" /></div>
        <div><label>Run interval (seconds; 0 = one-shot)</label><input type="number" min={0} max={3600} value={v.runIntervalSecs} onChange={(e) => upd("runIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Failure rate (0..1)</label><input type="number" step="any" min={0} max={1} value={v.failureRate} onChange={(e) => upd("failureRate", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Latency multiplier</label><input type="number" step="any" min={0.1} max={10} value={v.latencyMultiplier} onChange={(e) => upd("latencyMultiplier", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start test-run"}</button>
    </form>
  );
}
