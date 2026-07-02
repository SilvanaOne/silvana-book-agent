"use client";

import { useState } from "react";

export type FormValues = {
  markets: string;       // CSV
  side: "buy" | "sell";
  amountPerOrder: string;
  intervalSecs: string;
  priceOffsetPct: string;
  maxTotal: string;      // "" = unlimited
  startingPrice: string; // applied to all markets
};

const DEFAULTS: FormValues = {
  markets: "CBTC-CC,CETH-CC",
  side: "buy",
  amountPerOrder: "0.001",
  intervalSecs: "5",
  priceOffsetPct: "-0.5",
  maxTotal: "100",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function DcaPortfolioForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) {
    setV((p) => ({ ...p, [k]: val }));
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
      <div>
        <label>Markets (comma-separated)</label>
        <input
          value={v.markets}
          onChange={(e) => upd("markets", e.target.value)}
          disabled={disabled || busy}
          placeholder="CBTC-CC,CETH-CC"
        />
      </div>
      <div className="grid-2">
        <div>
          <label>Side</label>
          <select
            value={v.side}
            onChange={(e) => upd("side", e.target.value as "buy" | "sell")}
            disabled={disabled || busy}
          >
            <option value="buy">buy (BID)</option>
            <option value="sell">sell (OFFER)</option>
          </select>
        </div>
        <div>
          <label>Amount per order (per market)</label>
          <input
            type="number"
            step="any"
            value={v.amountPerOrder}
            onChange={(e) => upd("amountPerOrder", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Interval (secs)</label>
          <input
            type="number"
            step="1"
            value={v.intervalSecs}
            onChange={(e) => upd("intervalSecs", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
        <div>
          <label>Price offset (%)</label>
          <input
            type="number"
            step="any"
            value={v.priceOffsetPct}
            onChange={(e) => upd("priceOffsetPct", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Max total per market (optional)</label>
          <input
            type="number"
            step="any"
            value={v.maxTotal}
            onChange={(e) => upd("maxTotal", e.target.value)}
            disabled={disabled || busy}
            placeholder="empty = unlimited"
          />
        </div>
        <div>
          <label>Starting price (mid seed)</label>
          <input
            type="number"
            step="any"
            value={v.startingPrice}
            onChange={(e) => upd("startingPrice", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
      </div>
      {err && (
        <div className="negative mono" style={{ fontSize: 13 }}>
          {err}
        </div>
      )}
      <button type="submit" className="primary" disabled={disabled || busy}>
        {busy ? "Starting…" : "Start DCA Portfolio"}
      </button>
    </form>
  );
}
