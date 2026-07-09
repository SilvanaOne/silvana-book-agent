"use client";

import { useState } from "react";

export type FormValues = {
  targetsJson: string;
  upperBandPct: string;
  lowerBandPct: string;
  priceOffsetPct: string;
  checkIntervalSecs: string;
  startingPrice: string;
};

const DEFAULT_TARGETS = JSON.stringify(
  [
    { instrument: "Amulet", market: "CC-USDC", targetWeight: 0.4, startBalance: 100 },
    { instrument: "CBTC", market: "CBTC-CC", targetWeight: 0.6, startBalance: 0.005 },
  ],
  null,
  2,
);

const DEFAULTS: FormValues = {
  targetsJson: DEFAULT_TARGETS,
  upperBandPct: "2.0",
  lowerBandPct: "2.0",
  priceOffsetPct: "0.0",
  checkIntervalSecs: "5",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function PortfolioRebalancingForm({ disabled, onStart }: Props) {
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
        <label>Targets (JSON array)</label>
        <textarea
          value={v.targetsJson}
          onChange={(e) => upd("targetsJson", e.target.value)}
          disabled={disabled || busy}
          rows={8}
          className="mono"
          style={{
            width: "100%",
            fontSize: 12,
            padding: 8,
            background: "var(--bg-card)",
            color: "var(--text)",
            border: "1px solid var(--border)",
            borderRadius: 6,
            resize: "vertical",
          }}
        />
        <div className="muted" style={{ fontSize: 11, marginTop: 4 }}>
          {'{ instrument, market, targetWeight (0-1), startBalance }.  Weights must sum to 1.'}
        </div>
      </div>
      <div className="grid-2">
        <div><label>Upper band (pp above target)</label><input type="number" step="any" value={v.upperBandPct} onChange={(e) => upd("upperBandPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Lower band (pp below target)</label><input type="number" step="any" value={v.lowerBandPct} onChange={(e) => upd("lowerBandPct", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Price offset (% of mid)</label><input type="number" step="any" value={v.priceOffsetPct} onChange={(e) => upd("priceOffsetPct", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Check interval (secs)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (base mid)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting..." : "Start Rebalancing"}</button>
    </form>
  );
}
