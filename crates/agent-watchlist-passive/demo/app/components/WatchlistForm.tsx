"use client";

import { useState } from "react";

export type FormValues = {
  markets: string;          // CSV
  depthLevels: string;
  includePrices: boolean;
  includeOrderbook: boolean;
  pollSecs: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  markets: "CC-USDC,BTC-USD,CETH-CC",
  depthLevels: "10",
  includePrices: true,
  includeOrderbook: true,
  pollSecs: "2",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function WatchlistForm({ disabled, onStart }: Props) {
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
        <label>Markets (comma-separated)</label>
        <input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} />
      </div>
      <div className="grid-2">
        <div><label>Depth levels</label><input type="number" step="1" value={v.depthLevels} onChange={(e) => upd("depthLevels", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Poll interval (secs)</label><input type="number" step="any" value={v.pollSecs} onChange={(e) => upd("pollSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <input type="checkbox" checked={v.includePrices} onChange={(e) => upd("includePrices", e.target.checked)} disabled={disabled || busy} style={{ width: "auto" }} />
            Include prices stream
          </label>
        </div>
        <div>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <input type="checkbox" checked={v.includeOrderbook} onChange={(e) => upd("includeOrderbook", e.target.checked)} disabled={disabled || busy} style={{ width: "auto" }} />
            Include orderbook depth
          </label>
        </div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (fallback seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Watchlist"}</button>
    </form>
  );
}
