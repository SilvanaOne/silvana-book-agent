"use client";

import { useState } from "react";

export type FormValues = {
  threshold: string;
  quantity: string;
  headlinesPerSec: string;
  aliases: string;
  headlinePool: string;
};

const DEFAULT_ALIASES = `BTC-USD=bitcoin,btc
CC-USDC=canton,cc
ETH-USD=ethereum,eth`;

const DEFAULT_POOL = `Massive Bitcoin rally sends BTC to record high
Canton network adopts new consensus, positive news
Ethereum plunges after SEC investigation
BTC bearish news, huge dump expected
Canton team announces breakthrough — bullish
ETH crashes on regulatory concerns
Unrelated tech announcement about AI
BTC record low, market panic
Canton beats expectations, rally continues`;

const DEFAULTS: FormValues = {
  threshold: "0.3",
  quantity: "1.0",
  headlinesPerSec: "1",
  aliases: DEFAULT_ALIASES,
  headlinePool: DEFAULT_POOL,
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function NewsForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>|Score| threshold [0..1]</label><input type="number" step="any" min={0} max={1} value={v.threshold} onChange={(e) => upd("threshold", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Quantity per emitted signal</label><input type="number" step="any" min={0.01} value={v.quantity} onChange={(e) => upd("quantity", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Headlines per second</label><input type="number" step="any" min={0.1} max={10} value={v.headlinesPerSec} onChange={(e) => upd("headlinesPerSec", e.target.value)} disabled={disabled || busy} /></div>
      <div><label>Market aliases (one per line: MARKET=alias1,alias2)</label>
        <textarea value={v.aliases} onChange={(e) => upd("aliases", e.target.value)} disabled={disabled || busy} rows={4}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }} /></div>
      <div><label>Headline pool (one per line)</label>
        <textarea value={v.headlinePool} onChange={(e) => upd("headlinePool", e.target.value)} disabled={disabled || busy} rows={8}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }} /></div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start scoring"}</button>
    </form>
  );
}
