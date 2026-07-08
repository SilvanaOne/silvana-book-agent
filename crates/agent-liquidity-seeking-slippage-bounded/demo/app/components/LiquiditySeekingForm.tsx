"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  side: "buy" | "sell";
  total: string;
  maxSlippageBps: string;
  maxChunk: string;
  depth: string;
  maxRuntimeSecs: string;
  spreadBps: string;
  depthFalloffPct: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  side: "buy",
  total: "100",
  maxSlippageBps: "25",
  maxChunk: "5",
  depth: "20",
  maxRuntimeSecs: "600",
  spreadBps: "10",
  depthFalloffPct: "0.5",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function LiquiditySeekingForm({ disabled, onStart }: Props) {
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
        <div><label>Market</label><input value={v.market} onChange={(e) => upd("market", e.target.value)} disabled={disabled || busy} /></div>
        <div>
          <label>Side</label>
          <select value={v.side} onChange={(e) => upd("side", e.target.value as "buy" | "sell")} disabled={disabled || busy}>
            <option value="buy">buy</option>
            <option value="sell">sell</option>
          </select>
        </div>
      </div>
      <div className="grid-2">
        <div><label>Total parent qty</label><input type="number" step="any" value={v.total} onChange={(e) => upd("total", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max slippage (bps)</label><input type="number" step="any" value={v.maxSlippageBps} onChange={(e) => upd("maxSlippageBps", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Max chunk (per child)</label><input type="number" step="any" value={v.maxChunk} onChange={(e) => upd("maxChunk", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Depth levels</label><input type="number" step="1" value={v.depth} onChange={(e) => upd("depth", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Max runtime (secs)</label><input type="number" step="1" value={v.maxRuntimeSecs} onChange={(e) => upd("maxRuntimeSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Book spread (bps)</label><input type="number" step="any" value={v.spreadBps} onChange={(e) => upd("spreadBps", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Depth falloff (% / level)</label><input type="number" step="any" value={v.depthFalloffPct} onChange={(e) => upd("depthFalloffPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Liquidity Seeking"}</button>
    </form>
  );
}
