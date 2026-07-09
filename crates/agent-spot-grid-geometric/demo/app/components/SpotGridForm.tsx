"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  midPrice: string;
  bidLevels: string;
  offerLevels: string;
  baseStepPct: string;
  ratio: string;
  qtyPerLevel: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  midPrice: "0.15",
  bidLevels: "5",
  offerLevels: "5",
  baseStepPct: "0.5",
  ratio: "1.5",
  qtyPerLevel: "1",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function SpotGridForm({ disabled, onStart }: Props) {
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
        <div><label>Mid price (grid center)</label><input type="number" step="any" value={v.midPrice} onChange={(e) => upd("midPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Bid levels</label><input type="number" step="1" value={v.bidLevels} onChange={(e) => upd("bidLevels", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Offer levels</label><input type="number" step="1" value={v.offerLevels} onChange={(e) => upd("offerLevels", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Base step % (level 1)</label><input type="number" step="any" value={v.baseStepPct} onChange={(e) => upd("baseStepPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Ratio (≥ 1)</label><input type="number" step="any" value={v.ratio} onChange={(e) => upd("ratio", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Qty / level</label><input type="number" step="any" value={v.qtyPerLevel} onChange={(e) => upd("qtyPerLevel", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Geometric Grid"}</button>
    </form>
  );
}
