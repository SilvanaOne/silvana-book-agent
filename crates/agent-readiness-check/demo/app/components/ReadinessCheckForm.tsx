"use client";

import { useState } from "react";

export type FormValues = {
  requiredBalances: string; // JSON textarea
  maxFailedSettlements: string;
  maxPendingSettlements: string;
  requirePreapproval: boolean;
  checkIntervalSecs: string;
  driftEnabled: boolean;
  startingPrice: string;
  initialFailed: string;
  initialPending: string;
};

const DEFAULT_REQUIRED = '[{"instrument":"Amulet","minAmount":50},{"instrument":"CC-USDC","minAmount":100}]';

const DEFAULTS: FormValues = {
  requiredBalances: DEFAULT_REQUIRED,
  maxFailedSettlements: "0",
  maxPendingSettlements: "10",
  requirePreapproval: true,
  checkIntervalSecs: "5",
  driftEnabled: true,
  startingPrice: "0.15",
  initialFailed: "0",
  initialPending: "2",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ReadinessCheckForm({ disabled, onStart }: Props) {
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
        <label>Required balances (JSON)</label>
        <textarea
          value={v.requiredBalances}
          onChange={(e) => upd("requiredBalances", e.target.value)}
          disabled={disabled || busy}
          rows={3}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12 }}
        />
      </div>
      <div className="grid-2">
        <div><label>Max failed settlements</label><input type="number" step="1" value={v.maxFailedSettlements} onChange={(e) => upd("maxFailedSettlements", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max pending settlements</label><input type="number" step="1" value={v.maxPendingSettlements} onChange={(e) => upd("maxPendingSettlements", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Check interval (secs)</label><input type="number" step="1" value={v.checkIntervalSecs} onChange={(e) => upd("checkIntervalSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Initial failed</label><input type="number" step="1" value={v.initialFailed} onChange={(e) => upd("initialFailed", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Initial pending</label><input type="number" step="1" value={v.initialPending} onChange={(e) => upd("initialPending", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <input type="checkbox" checked={v.requirePreapproval} onChange={(e) => upd("requirePreapproval", e.target.checked)} disabled={disabled || busy} />
            Require TransferPreapproval
          </label>
        </div>
        <div>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <input type="checkbox" checked={v.driftEnabled} onChange={(e) => upd("driftEnabled", e.target.checked)} disabled={disabled || busy} />
            Drift synthetic state
          </label>
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Readiness Check"}</button>
    </form>
  );
}
