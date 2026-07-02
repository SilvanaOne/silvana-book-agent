"use client";

import { useState } from "react";

export type FormValues = {
  orders: boolean;
  settlements: boolean;
  market: string;
  eventArrivalPerTick: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  orders: true,
  settlements: true,
  market: "",
  eventArrivalPerTick: "0.3",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function TradingHistoryForm({ disabled, onStart }: Props) {
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
        <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <input type="checkbox" checked={v.orders} onChange={(e) => upd("orders", e.target.checked)} disabled={disabled || busy} />
          Subscribe to orders
        </label>
        <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <input type="checkbox" checked={v.settlements} onChange={(e) => upd("settlements", e.target.checked)} disabled={disabled || busy} />
          Subscribe to settlements
        </label>
      </div>
      <div className="grid-2">
        <div>
          <label>Market filter (optional)</label>
          <input placeholder="e.g. CC-USDC (blank = all)" value={v.market} onChange={(e) => upd("market", e.target.value)} disabled={disabled || busy} />
        </div>
        <div>
          <label>Event arrival λ / tick</label>
          <input type="number" step="any" min="0" max="10" value={v.eventArrivalPerTick} onChange={(e) => upd("eventArrivalPerTick", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Starting price (mid seed)</label>
          <input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} />
        </div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Trading History"}</button>
    </form>
  );
}
