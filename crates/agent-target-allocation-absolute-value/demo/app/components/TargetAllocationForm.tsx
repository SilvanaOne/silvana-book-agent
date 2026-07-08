"use client";

import { useState } from "react";
import { DEFAULT_TARGETS } from "@/lib/targetallocation-engine";

export type FormValues = {
  targets: string;
  thresholdQuote: string;
  rebalanceFraction: string;
  checkIntervalSecs: string;
  startingPrice: string;
};

const DEFAULT_TARGETS_JSON = JSON.stringify([...DEFAULT_TARGETS], null, 2);

const DEFAULTS: FormValues = {
  targets: DEFAULT_TARGETS_JSON,
  thresholdQuote: "100",
  rebalanceFraction: "0.5",
  checkIntervalSecs: "5",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function TargetAllocationForm({ disabled, onStart }: Props) {
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
        <label>Targets (JSON array of {`{instrument, market, targetQuote, startBalance, priceMultiplier}`})</label>
        <textarea
          value={v.targets}
          onChange={(e) => upd("targets", e.target.value)}
          disabled={disabled || busy}
          rows={8}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12 }}
        />
      </div>
      <div className="grid-2">
        <div><label>Threshold (quote)</label><input type="number" step="any" value={v.thresholdQuote} onChange={(e) => upd("thresholdQuote", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Rebalance fraction (0-1]</label><input type="number" step="any" value={v.rebalanceFraction} onChange={(e) => upd("rebalanceFraction", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Check interval (secs)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (base seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Target Allocation"}</button>
    </form>
  );
}
