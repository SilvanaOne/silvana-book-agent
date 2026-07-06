"use client";

import { useState } from "react";

export type FormValues = {
  historySize: string;
  parties: string;
  markets: string;
  blockedMarkets: string;
  allowedMarkets: string;
  allowedSides: string;
  maxNotionalPerOrder: string;
  maxQuantityPerOrder: string;
  minPrice: string;
  maxPrice: string;
  perMarketMaxDailyNotional: string;
  emitAccepts: boolean;
};

const DEFAULTS: FormValues = {
  historySize: "150",
  parties: "party::alice,party::bob,party::charlie,party::whale",
  markets: "CC-USDC,BTC-USD,SCAM-USDC",
  blockedMarkets: "SCAM-USDC",
  allowedMarkets: "",
  allowedSides: "",
  maxNotionalPerOrder: "500000",
  maxQuantityPerOrder: "",
  minPrice: "0.1",
  maxPrice: "70000",
  perMarketMaxDailyNotional: "BTC-USD=2000000",
  emitAccepts: false,
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ReplayForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>History size</label><input type="number" min={20} max={500} value={v.historySize} onChange={(e) => upd("historySize", e.target.value)} disabled={disabled || busy} /></div>
        <div>
          <label>&nbsp;</label>
          <label className="row" style={{ gap: 8, alignItems: "center", paddingTop: 6, fontSize: 13.5 }}>
            <input type="checkbox" checked={v.emitAccepts} onChange={(e) => upd("emitAccepts", e.target.checked)} disabled={disabled || busy} />
            <span><span className="mono">--emit-accepts</span> (default: rejects only)</span>
          </label>
        </div>
      </div>
      <div><label>Party pool</label><input value={v.parties} onChange={(e) => upd("parties", e.target.value)} disabled={disabled || busy} /></div>
      <div><label>Market pool</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
      <div className="grid-2">
        <div><label>Blocked markets</label><input value={v.blockedMarkets} onChange={(e) => upd("blockedMarkets", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Allowed markets (empty = all)</label><input value={v.allowedMarkets} onChange={(e) => upd("allowedMarkets", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Max notional / order</label><input type="number" step="any" value={v.maxNotionalPerOrder} onChange={(e) => upd("maxNotionalPerOrder", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max quantity / order</label><input type="number" step="any" value={v.maxQuantityPerOrder} onChange={(e) => upd("maxQuantityPerOrder", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Min price</label><input type="number" step="any" value={v.minPrice} onChange={(e) => upd("minPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max price</label><input type="number" step="any" value={v.maxPrice} onChange={(e) => upd("maxPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Per-market daily notional cap (MARKET=CAP; one per line)</label>
        <textarea value={v.perMarketMaxDailyNotional} onChange={(e) => upd("perMarketMaxDailyNotional", e.target.value)} disabled={disabled || busy} rows={2}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }} />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Running…" : "Load history + replay"}</button>
    </form>
  );
}
