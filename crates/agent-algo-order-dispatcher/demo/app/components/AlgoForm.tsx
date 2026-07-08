"use client";

import { useState } from "react";

export type FormValues = {
  plan: string;
  startingPrice: string;
  bookVolatility: string;
};

const DEFAULT_PLAN = `algorithm=twap
market=CC-USDC side=buy total=50 slices=10 duration_secs=30 price_offset_pct=0

algorithm=iceberg
market=CC-USDC side=sell total=30 visible=3 price=1.02 poll_secs=2

algorithm=liquidity-seeking
market=BTC-USD side=buy total=2 max_chunk=0.4 max_slippage_bps=25 depth=20 poll_secs=2`;

const DEFAULTS: FormValues = {
  plan: DEFAULT_PLAN,
  startingPrice: "1.0",
  bookVolatility: "0.4",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function AlgoForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setErr(null);
    setBusy(true);
    try { await onStart({ ...v, bookVolatility: String(Number(v.bookVolatility) / 100) }); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); }
  }

  return (
    <form onSubmit={submit} className="stack">
      <div>
        <label>
          Plan (steps separated by blank lines; each has key=value tokens)
          <br />
          <span className="mono" style={{ fontSize: 11 }}>algorithm ∈ &#123;twap · iceberg · liquidity-seeking&#125;</span>
        </label>
        <textarea
          value={v.plan}
          onChange={(e) => upd("plan", e.target.value)}
          disabled={disabled || busy}
          rows={10}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }}
        />
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Book volatility (%)</label><input type="number" step="any" min={0} max={10} value={v.bookVolatility} onChange={(e) => upd("bookVolatility", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start dispatcher"}</button>
    </form>
  );
}
