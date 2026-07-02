"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  side: "buy" | "sell";
  total: string;
  price: string;
  timeSlices: string;
  durationSecs: string;
  visible: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  side: "sell",
  total: "100",
  price: "0.155",
  timeSlices: "10",
  durationSecs: "600",
  visible: "2",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function BlockExecutionForm({ disabled, onStart }: Props) {
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

  // When side flips, nudge default limit price to a sensible sample if user
  // hasn't already changed it.
  function flipSide(next: "buy" | "sell") {
    setV((p) => ({
      ...p,
      side: next,
      price: next === "buy" ? (p.price === "0.155" ? "0.145" : p.price) : (p.price === "0.145" ? "0.155" : p.price),
    }));
  }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>Market</label><input value={v.market} onChange={(e) => upd("market", e.target.value)} disabled={disabled || busy} /></div>
        <div>
          <label>Side</label>
          <div className="row" style={{ gap: 6 }}>
            <button type="button" className={v.side === "buy" ? "primary" : "ghost"} onClick={() => flipSide("buy")} disabled={disabled || busy} style={{ flex: 1 }}>buy</button>
            <button type="button" className={v.side === "sell" ? "primary" : "ghost"} onClick={() => flipSide("sell")} disabled={disabled || busy} style={{ flex: 1 }}>sell</button>
          </div>
        </div>
      </div>
      <div className="grid-2">
        <div><label>Total (parent qty)</label><input type="number" step="any" value={v.total} onChange={(e) => upd("total", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Limit price / chunk</label><input type="number" step="any" value={v.price} onChange={(e) => upd("price", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Time slices</label><input type="number" step="1" value={v.timeSlices} onChange={(e) => upd("timeSlices", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Duration (secs)</label><input type="number" step="1" value={v.durationSecs} onChange={(e) => upd("durationSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Visible / chunk</label><input type="number" step="any" value={v.visible} onChange={(e) => upd("visible", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Block Execution"}</button>
    </form>
  );
}
