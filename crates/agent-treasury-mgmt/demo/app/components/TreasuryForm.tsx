"use client";

import { useState } from "react";

export type FormValues = {
  targets: string;
  approvalThresholdQuote: string;
  maxTradeQuote: string;
  maxDailyTradeQuote: string;
  thresholdQuote: string;
  rebalanceFraction: string;
  checkIntervalSecs: string;
  balanceDriftPerCycle: string;
};

const DEFAULT_TARGETS = `instrument=Amulet market=CC-USDC target=10000 balance=9000 price=1.0
instrument=CBTC market=CBTC-CC target=20000 balance=0.32 price=60000
instrument=CETH market=CETH-CC target=5000 balance=1.4 price=3500`;

const DEFAULTS: FormValues = {
  targets: DEFAULT_TARGETS,
  approvalThresholdQuote: "500",
  maxTradeQuote: "2500",
  maxDailyTradeQuote: "8000",
  thresholdQuote: "100",
  rebalanceFraction: "0.4",
  checkIntervalSecs: "6",
  balanceDriftPerCycle: "0.2",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function TreasuryForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div>
        <label>Targets (one per line: instrument= market= target= [balance=] [price=])</label>
        <textarea value={v.targets} onChange={(e) => upd("targets", e.target.value)} disabled={disabled || busy} rows={5}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }} />
      </div>
      <div className="grid-2">
        <div><label>Approval threshold (quote)</label><input type="number" step="any" min={0} value={v.approvalThresholdQuote} onChange={(e) => upd("approvalThresholdQuote", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max per trade (quote)</label><input type="number" step="any" min={0} value={v.maxTradeQuote} onChange={(e) => upd("maxTradeQuote", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Max 24h total (quote)</label><input type="number" step="any" min={0} value={v.maxDailyTradeQuote} onChange={(e) => upd("maxDailyTradeQuote", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Trigger threshold (quote)</label><input type="number" step="any" min={0} value={v.thresholdQuote} onChange={(e) => upd("thresholdQuote", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Rebalance fraction (0..1)</label><input type="number" step="any" min={0.01} max={1} value={v.rebalanceFraction} onChange={(e) => upd("rebalanceFraction", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Check interval (seconds)</label><input type="number" min={1} max={300} value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Balance drift / cycle</label><input type="number" step="any" value={v.balanceDriftPerCycle} onChange={(e) => upd("balanceDriftPerCycle", e.target.value)} disabled={disabled || busy} /></div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start monitoring"}</button>
    </form>
  );
}
