"use client";

import { useState } from "react";

export type FormValues = {
  role: string;
  ttlSecs: string;
  autoRefreshEnabled: boolean;
  autoRefreshIntervalSecs: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  role: "trader",
  ttlSecs: "3600",
  autoRefreshEnabled: true,
  autoRefreshIntervalSecs: "60",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function AuthForm({ disabled, onStart }: Props) {
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
        <div><label>Role</label><input value={v.role} onChange={(e) => upd("role", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>TTL (secs)</label><input type="number" step="1" value={v.ttlSecs} onChange={(e) => upd("ttlSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div>
          <label>Auto-refresh</label>
          <label style={{ display: "flex", alignItems: "center", gap: 8, fontWeight: 400, fontSize: 13 }}>
            <input type="checkbox" checked={v.autoRefreshEnabled} onChange={(e) => upd("autoRefreshEnabled", e.target.checked)} disabled={disabled || busy} />
            <span>rotate token on cadence</span>
          </label>
        </div>
        <div><label>Refresh interval (secs)</label><input type="number" step="1" value={v.autoRefreshIntervalSecs} onChange={(e) => upd("autoRefreshIntervalSecs", e.target.value)} disabled={disabled || busy || !v.autoRefreshEnabled} /></div>
      </div>
      <div className="grid-2">
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Auth"}</button>
    </form>
  );
}
