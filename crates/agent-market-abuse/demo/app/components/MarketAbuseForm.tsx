"use client";

import { useState } from "react";

export type FormValues = {
  market: string;
  spoofBurst: string;
  spoofBurstWindowSecs: string;
  spoofWindowSecs: string;
  layerMinOrders: string;
  layerPriceBandPct: string;
  orderArrivalPerTick: string;
  cancelRatePerTick: string;
  spoofScenarioEnabled: boolean;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  market: "CC-USDC",
  spoofBurst: "10",
  spoofBurstWindowSecs: "10",
  spoofWindowSecs: "1",
  layerMinOrders: "5",
  layerPriceBandPct: "1.0",
  orderArrivalPerTick: "0.4",
  cancelRatePerTick: "0.1",
  spoofScenarioEnabled: true,
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function MarketAbuseForm({ disabled, onStart }: Props) {
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
        <div><label>Market</label><input value={v.market} onChange={(e) => upd("market", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Spoof burst</label><input type="number" step="1" value={v.spoofBurst} onChange={(e) => upd("spoofBurst", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Spoof burst window (s)</label><input type="number" step="1" value={v.spoofBurstWindowSecs} onChange={(e) => upd("spoofBurstWindowSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Spoof window (s, create→cancel)</label><input type="number" step="any" value={v.spoofWindowSecs} onChange={(e) => upd("spoofWindowSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Layer min orders</label><input type="number" step="1" value={v.layerMinOrders} onChange={(e) => upd("layerMinOrders", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Layer price band (%)</label><input type="number" step="any" value={v.layerPriceBandPct} onChange={(e) => upd("layerPriceBandPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Order arrival / tick (Poisson λ)</label><input type="number" step="any" value={v.orderArrivalPerTick} onChange={(e) => upd("orderArrivalPerTick", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Cancel rate / tick (0..1)</label><input type="number" step="any" value={v.cancelRatePerTick} onChange={(e) => upd("cancelRatePerTick", e.target.value)} disabled={disabled || busy} /></div>
        <div style={{ display: "flex", alignItems: "flex-end", gap: 8 }}>
          <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
            <input type="checkbox" checked={v.spoofScenarioEnabled} onChange={(e) => upd("spoofScenarioEnabled", e.target.checked)} disabled={disabled || busy} />
            Spoof scenario enabled
          </label>
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Market Abuse"}</button>
    </form>
  );
}
