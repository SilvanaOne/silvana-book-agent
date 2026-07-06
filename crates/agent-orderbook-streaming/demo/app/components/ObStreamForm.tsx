"use client";

import { useState } from "react";

export type FormValues = {
  markets: string;
  depth: string;
  startingPrices: string;
  source: string;
  includeOrderbook: boolean;
  includeTrades: boolean;
  noDepth: boolean;
  noPrices: boolean;
  sinks: string;
  webhookFailureRate: string;
};

const DEFAULTS: FormValues = {
  markets: "CC-USDC,BTC-USD",
  depth: "10",
  startingPrices: "0.15,60000",
  source: "silvana.oracle",
  includeOrderbook: false,
  includeTrades: true,
  noDepth: false,
  noPrices: false,
  sinks: "stdout\nfile:ticks.jsonl\nwebhook:https://hooks.example.com/feed",
  webhookFailureRate: "0.05",
};

const SOURCES = ["silvana.oracle", "binance_spot", "bybit", "coingecko"] as const;

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ObStreamForm({ disabled, onStart }: Props) {
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
        <div><label>Markets (comma-separated, BASE-QUOTE)</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Depth levels per side</label><input type="number" min={1} max={30} value={v.depth} onChange={(e) => upd("depth", e.target.value)} disabled={disabled || busy} /></div>
      </div>

      <div className="grid-2">
        <div><label>Starting prices (comma-separated, per market)</label><input value={v.startingPrices} onChange={(e) => upd("startingPrices", e.target.value)} disabled={disabled || busy} /></div>
        <div>
          <label>External source</label>
          <select value={v.source} onChange={(e) => upd("source", e.target.value)} disabled={disabled || busy}>
            {SOURCES.map((s) => <option key={s} value={s}>{s}</option>)}
          </select>
        </div>
      </div>

      <div>
        <label>Stream toggles</label>
        <div className="row" style={{ gap: 14, flexWrap: "wrap", fontSize: 13.5 }}>
          <label className="row" style={{ gap: 6, alignItems: "center" }}>
            <input type="checkbox" checked={!v.noPrices} onChange={(e) => upd("noPrices", !e.target.checked)} disabled={disabled || busy} />
            <span>prices (StreamPrices)</span>
          </label>
          <label className="row" style={{ gap: 6, alignItems: "center" }}>
            <input type="checkbox" checked={!v.noDepth} onChange={(e) => upd("noDepth", !e.target.checked)} disabled={disabled || busy} />
            <span>depth (SubscribeOrderbookDepth)</span>
          </label>
          <label className="row" style={{ gap: 6, alignItems: "center" }}>
            <input type="checkbox" checked={v.includeOrderbook} onChange={(e) => upd("includeOrderbook", e.target.checked)} disabled={disabled || busy} />
            <span>include-orderbook</span>
          </label>
          <label className="row" style={{ gap: 6, alignItems: "center" }}>
            <input type="checkbox" checked={v.includeTrades} onChange={(e) => upd("includeTrades", e.target.checked)} disabled={disabled || busy} />
            <span>include-trades</span>
          </label>
        </div>
      </div>

      <div>
        <label>Sinks (one per line: <span className="mono">stdout</span> · <span className="mono">file:PATH</span> · <span className="mono">webhook:URL</span>)</label>
        <textarea
          value={v.sinks}
          onChange={(e) => upd("sinks", e.target.value)}
          disabled={disabled || busy}
          rows={4}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12.5, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }}
        />
      </div>

      <div className="grid-2">
        <div><label>Webhook failure rate (0..1)</label><input type="number" step="any" value={v.webhookFailureRate} onChange={(e) => upd("webhookFailureRate", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>

      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start streaming"}</button>
    </form>
  );
}
