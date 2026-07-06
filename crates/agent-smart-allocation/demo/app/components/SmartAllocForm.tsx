"use client";

import { useState } from "react";

export type FormValues = {
  policy: string;
  bucketThresholdPct: string;
  rebalanceFraction: string;
  checkIntervalSecs: string;
  balanceDriftPerCycle: string;
};

const DEFAULT_POLICY = `bucket=stables weight=0.5
instrument=Amulet market=CC-USDC weight=1.0 balance=500 price=1.0

bucket=btc-theme weight=0.3
instrument=CBTC market=CBTC-CC weight=1.0 balance=0.008 price=60000

bucket=eth-theme weight=0.2
instrument=CETH market=CETH-CC weight=1.0 balance=0.15 price=3500`;

const DEFAULTS: FormValues = {
  policy: DEFAULT_POLICY,
  bucketThresholdPct: "2.0",
  rebalanceFraction: "0.5",
  checkIntervalSecs: "8",
  balanceDriftPerCycle: "2.0",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function SmartAllocForm({ disabled, onStart }: Props) {
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
        <label>
          Policy (buckets separated by blank line; per-line:
          <br />
          <span className="mono" style={{ fontSize: 11.5 }}>bucket=NAME weight=W</span> · <span className="mono" style={{ fontSize: 11.5 }}>instrument=NAME market=BASE-QUOTE weight=W balance=N price=N</span>)
        </label>
        <textarea
          value={v.policy}
          onChange={(e) => upd("policy", e.target.value)}
          disabled={disabled || busy}
          rows={12}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }}
        />
      </div>

      <div className="grid-2">
        <div><label>Bucket threshold (%)</label><input type="number" step="any" min={0} value={v.bucketThresholdPct} onChange={(e) => upd("bucketThresholdPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Rebalance fraction (0..1)</label><input type="number" step="any" min={0.01} max={1} value={v.rebalanceFraction} onChange={(e) => upd("rebalanceFraction", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Check interval (seconds)</label><input type="number" min={1} max={300} value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Balance drift / cycle (units)</label><input type="number" step="any" value={v.balanceDriftPerCycle} onChange={(e) => upd("balanceDriftPerCycle", e.target.value)} disabled={disabled || busy} /></div>
      </div>

      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start allocation"}</button>
    </form>
  );
}
