"use client";

import { useState } from "react";

export type FormValues = {
  markets: string;
  startingPrices: string;
  checkIntervalSecs: string;
  spawnPerCycle: string;
  enforce: boolean;
  maxOpenOrders: string;
  maxOpenNotional: string;
  maxPendingSettlements: string;
  maxFailedSettlements: string;
  perMarketMaxNotional: string;
};

const DEFAULTS: FormValues = {
  markets: "CC-USDC,BTC-USD",
  startingPrices: "0.15,60000",
  checkIntervalSecs: "10",
  spawnPerCycle: "4",
  enforce: true,
  maxOpenOrders: "40",
  maxOpenNotional: "1000",
  maxPendingSettlements: "5",
  maxFailedSettlements: "1",
  perMarketMaxNotional: "CC-USDC=600",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function RiskMgmtForm({ disabled, onStart }: Props) {
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
        <div><label>Markets (comma-separated)</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting prices (per market)</label><input value={v.startingPrices} onChange={(e) => upd("startingPrices", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Check interval (seconds)</label><input type="number" min={1} max={300} value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Spawn per cycle (avg new orders)</label><input type="number" step="any" min={0} max={50} value={v.spawnPerCycle} onChange={(e) => upd("spawnPerCycle", e.target.value)} disabled={disabled || busy} /></div>
      </div>

      <div style={{ borderTop: "1px solid var(--border)", paddingTop: 12, marginTop: 4 }}>
        <label style={{ fontWeight: 600 }}>Policy limits (leave blank to skip a check)</label>
        <div className="grid-2" style={{ marginTop: 8 }}>
          <div><label>max_open_orders</label><input type="number" min={0} value={v.maxOpenOrders} onChange={(e) => upd("maxOpenOrders", e.target.value)} disabled={disabled || busy} /></div>
          <div><label>max_open_notional (quote)</label><input type="number" step="any" min={0} value={v.maxOpenNotional} onChange={(e) => upd("maxOpenNotional", e.target.value)} disabled={disabled || busy} /></div>
        </div>
        <div className="grid-2">
          <div><label>max_pending_settlements</label><input type="number" min={0} value={v.maxPendingSettlements} onChange={(e) => upd("maxPendingSettlements", e.target.value)} disabled={disabled || busy} /></div>
          <div><label>max_failed_settlements</label><input type="number" min={0} value={v.maxFailedSettlements} onChange={(e) => upd("maxFailedSettlements", e.target.value)} disabled={disabled || busy} /></div>
        </div>
        <div>
          <label>per_market_max_notional (one per line, MARKET=CAP)</label>
          <textarea
            value={v.perMarketMaxNotional}
            onChange={(e) => upd("perMarketMaxNotional", e.target.value)}
            disabled={disabled || busy}
            rows={3}
            style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12.5, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }}
          />
        </div>
      </div>

      <label className="row" style={{ gap: 8, alignItems: "center", fontSize: 13.5 }}>
        <input type="checkbox" checked={v.enforce} onChange={(e) => upd("enforce", e.target.checked)} disabled={disabled || busy} />
        <span><span className="mono">--enforce</span> — cancel newest orders when limits breach</span>
      </label>

      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start monitoring"}</button>
    </form>
  );
}
