"use client";

import { useState } from "react";
import { CHART_PAIR_KEYS } from "@/lib/demo-data";

export type FormValues = {
  focusPair: string;
  minSpreadBps: string;
  tradeSizeUsd: string;
  scanIntervalSecs: string;
};

const DEFAULTS: FormValues = {
  focusPair: "CC/USDC",
  minSpreadBps: "200",
  tradeSizeUsd: "100",
  scanIntervalSecs: "4",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ArbitrageForm({ disabled, onStart }: Props) {
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
      <div className="grid-2">
        <div>
          <label>Focus pair (chart)</label>
          <select value={v.focusPair} onChange={(e) => upd("focusPair", e.target.value)} disabled={disabled || busy}>
            {CHART_PAIR_KEYS.map((p) => (
              <option key={p} value={p}>{p}</option>
            ))}
          </select>
        </div>
        <div>
          <label>Act threshold (bps)</label>
          <input type="number" step="1" value={v.minSpreadBps} onChange={(e) => upd("minSpreadBps", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Trade size (USD)</label>
          <input type="number" step="any" value={v.tradeSizeUsd} onChange={(e) => upd("tradeSizeUsd", e.target.value)} disabled={disabled || busy} />
        </div>
        <div>
          <label>Scan interval (secs)</label>
          <input type="number" step="1" value={v.scanIntervalSecs} onChange={(e) => upd("scanIntervalSecs", e.target.value)} disabled={disabled || busy} />
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start scanner"}</button>
    </form>
  );
}
