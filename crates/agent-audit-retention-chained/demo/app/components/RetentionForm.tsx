"use client";

import { useState } from "react";

export type FormValues = { totalDays: string; recordsPerDay: string; retentionDays: string; weekly: boolean; party: string };

const DEFAULTS: FormValues = { totalDays: "12", recordsPerDay: "40", retentionDays: "8", weekly: false, party: "party::alice" };

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function RetentionForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div><label>Party (embedded in slice header)</label><input value={v.party} onChange={(e) => upd("party", e.target.value)} disabled={disabled || busy} /></div>
      <div className="grid-2">
        <div><label>Total days of synthetic history</label><input type="number" min={2} max={60} value={v.totalDays} onChange={(e) => upd("totalDays", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Records / day</label><input type="number" min={5} max={200} value={v.recordsPerDay} onChange={(e) => upd("recordsPerDay", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Retention (days; empty = keep all)</label><input type="number" min={0} value={v.retentionDays} onChange={(e) => upd("retentionDays", e.target.value)} disabled={disabled || busy} /></div>
        <div>
          <label>&nbsp;</label>
          <label className="row" style={{ gap: 8, alignItems: "center", paddingTop: 6, fontSize: 13.5 }}>
            <input type="checkbox" checked={v.weekly} onChange={(e) => upd("weekly", e.target.checked)} disabled={disabled || busy} />
            <span>--weekly (default: daily buckets)</span>
          </label>
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Rotating…" : "Load + rotate"}</button>
    </form>
  );
}
