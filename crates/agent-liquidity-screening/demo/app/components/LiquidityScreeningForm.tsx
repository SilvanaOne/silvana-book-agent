"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  probeQty: string;
  depth: string;
  pollSecs: string;
  spreadBps: string;
  depthFalloffPct: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  probeQty: "10",
  depth: "10",
  pollSecs: "3",
  spreadBps: "10",
  depthFalloffPct: "0.5",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function LiquidityScreeningForm({ disabled, onStart }: Props) {
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
        <div><label>Probe qty</label><input type="number" step="any" value={v.probeQty} onChange={(e) => upd("probeQty", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Depth (levels / side)</label><input type="number" step="1" value={v.depth} onChange={(e) => upd("depth", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Poll interval (s)</label><input type="number" step="1" value={v.pollSecs} onChange={(e) => upd("pollSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Base spread (bps)</label><input type="number" step="any" value={v.spreadBps} onChange={(e) => upd("spreadBps", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Depth falloff (%/level)</label><input type="number" step="any" value={v.depthFalloffPct} onChange={(e) => upd("depthFalloffPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Liquidity"}</button>
    </form>
  );
}
