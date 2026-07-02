"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  side: "buy" | "sell";
  total: string;
  visible: string;
  price: string;
  maxRuntimeSecs: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  side: "sell",
  total: "100",
  visible: "2.5",
  price: "0.155",
  maxRuntimeSecs: "600",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function IcebergExecutionForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }

  function updateSide(newSide: "buy" | "sell") {
    // Nudge price default across sides so the demo produces fills.
    const start = Number(v.startingPrice);
    const price = newSide === "sell"
      ? (Number.isFinite(start) ? (start * 1.03).toFixed(4) : "0.155")
      : (Number.isFinite(start) ? (start * 0.97).toFixed(4) : "0.145");
    setV((p) => ({ ...p, side: newSide, price }));
  }

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
          <select value={v.side} onChange={(e) => updateSide(e.target.value as "buy" | "sell")} disabled={disabled || busy}>
            <option value="buy">buy</option>
            <option value="sell">sell</option>
          </select>
        </div>
      </div>
      <div className="grid-2">
        <div><label>Total parent qty</label><input type="number" step="any" value={v.total} onChange={(e) => upd("total", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Visible per chunk</label><input type="number" step="any" value={v.visible} onChange={(e) => upd("visible", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Chunk limit price</label><input type="number" step="any" value={v.price} onChange={(e) => upd("price", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max runtime (s)</label><input type="number" step="1" value={v.maxRuntimeSecs} onChange={(e) => upd("maxRuntimeSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Iceberg"}</button>
    </form>
  );
}
