"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  side: "long" | "short";
  quantity: string;
  entryPrice: string;
  tp: string;
  sl: string;
};

const DEFAULT_VALUES: FormValues = {
  market: "CC-USDC",
  side: "long",
  quantity: "10",
  entryPrice: "0.15",
  tp: "0.17",
  sl: "0.13",
};

type Props = Readonly<{
  disabled: boolean;
  onStart: (v: FormValues) => Promise<void> | void;
}>;

export function PositionForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULT_VALUES);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, value: FormValues[K]) {
    setV((prev) => ({ ...prev, [k]: value }));
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setErr(null);
    setBusy(true);
    try {
      await onStart(v);
    } catch (ex) {
      setErr((ex as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div>
          <label>Market</label>
          <input value={v.market} onChange={(e) => upd("market", e.target.value)} disabled={disabled || busy} />
        </div>
        <div>
          <label>Side</label>
          <select value={v.side} onChange={(e) => upd("side", e.target.value as "long" | "short")} disabled={disabled || busy}>
            <option value="long">LONG (bought earlier — bracket exits with OFFER)</option>
            <option value="short">SHORT (sold earlier — bracket exits with BID)</option>
          </select>
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Quantity (base)</label>
          <input type="number" step="any" value={v.quantity} onChange={(e) => upd("quantity", e.target.value)} disabled={disabled || busy} />
        </div>
        <div>
          <label>Entry price</label>
          <input type="number" step="any" value={v.entryPrice} onChange={(e) => upd("entryPrice", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Take Profit price</label>
          <input type="number" step="any" value={v.tp} onChange={(e) => upd("tp", e.target.value)} disabled={disabled || busy} />
        </div>
        <div>
          <label>Stop Loss price</label>
          <input type="number" step="any" value={v.sl} onChange={(e) => upd("sl", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>
      <div className="muted" style={{ fontSize: 12 }}>
        Both TP and SL are placed as resting limit orders (OCO). Whichever fills first closes the position; the other is cancelled.
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>
        {busy ? "Starting…" : "Place bracket"}
      </button>
    </form>
  );
}
