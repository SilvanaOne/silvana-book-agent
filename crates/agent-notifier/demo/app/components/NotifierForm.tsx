"use client";

import { useState } from "react";

export type FormValues = {
  orders: boolean;
  settlements: boolean;
  prices: boolean;
  sinks: string;              // multiline textarea, one entry per line
  market: string;             // optional
  webhookFailureRate: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  orders: true,
  settlements: true,
  prices: true,
  sinks: "stdout\nfile:events.jsonl\nwebhook:https://hooks.example.com/sink",
  market: "CC-USDC",
  webhookFailureRate: "0.05",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function NotifierForm({ disabled, onStart }: Props) {
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
        <label>Streams</label>
        <div className="row" style={{ gap: 14, flexWrap: "wrap", fontSize: 13.5 }}>
          <label className="row" style={{ gap: 6, alignItems: "center" }}>
            <input type="checkbox" checked={v.orders} onChange={(e) => upd("orders", e.target.checked)} disabled={disabled || busy} />
            <span>orders</span>
          </label>
          <label className="row" style={{ gap: 6, alignItems: "center" }}>
            <input type="checkbox" checked={v.settlements} onChange={(e) => upd("settlements", e.target.checked)} disabled={disabled || busy} />
            <span>settlements</span>
          </label>
          <label className="row" style={{ gap: 6, alignItems: "center" }}>
            <input type="checkbox" checked={v.prices} onChange={(e) => upd("prices", e.target.checked)} disabled={disabled || busy} />
            <span>prices</span>
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
        <div><label>Market filter (optional)</label><input value={v.market} onChange={(e) => upd("market", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Webhook failure rate (0..1)</label><input type="number" step="any" value={v.webhookFailureRate} onChange={(e) => upd("webhookFailureRate", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Notifier"}</button>
    </form>
  );
}
