"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  includeOrders: boolean;
  includeSettlements: boolean;
  orderArrivalPerTick: string;
  settlementArrivalPerTick: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "",
  includeOrders: true,
  includeSettlements: true,
  orderArrivalPerTick: "0.3",
  settlementArrivalPerTick: "0.15",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function StateMonitorForm({ disabled, onStart }: Props) {
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
        <div>
          <label>Market filter</label>
          <input
            placeholder="All markets"
            value={v.market}
            onChange={(e) => upd("market", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
        <div>
          <label>Starting price (mid seed)</label>
          <input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>

      <div className="grid-2">
        <div>
          <label style={{ display: "flex", gap: 8, alignItems: "center", cursor: "pointer" }}>
            <input
              type="checkbox"
              checked={v.includeOrders}
              onChange={(e) => upd("includeOrders", e.target.checked)}
              disabled={disabled || busy}
              style={{ width: "auto" }}
            />
            <span>Include orders stream</span>
          </label>
        </div>
        <div>
          <label style={{ display: "flex", gap: 8, alignItems: "center", cursor: "pointer" }}>
            <input
              type="checkbox"
              checked={v.includeSettlements}
              onChange={(e) => upd("includeSettlements", e.target.checked)}
              disabled={disabled || busy}
              style={{ width: "auto" }}
            />
            <span>Include settlements stream</span>
          </label>
        </div>
      </div>

      <div className="grid-2">
        <div>
          <label>Order arrival / tick</label>
          <input type="number" step="any" min="0" max="10" value={v.orderArrivalPerTick} onChange={(e) => upd("orderArrivalPerTick", e.target.value)} disabled={disabled || busy} />
        </div>
        <div>
          <label>Settlement arrival / tick</label>
          <input type="number" step="any" min="0" max="10" value={v.settlementArrivalPerTick} onChange={(e) => upd("settlementArrivalPerTick", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>

      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start State Monitor"}</button>
    </form>
  );
}
