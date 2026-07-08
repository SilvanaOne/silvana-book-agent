"use client";

import { useState } from "react";

export type FormValues = { party: string; historyGrowthPerSec: string; intervalSecs: string; webhookFailureRate: string };

const DEFAULTS: FormValues = { party: "party::alice", historyGrowthPerSec: "2", intervalSecs: "10", webhookFailureRate: "0.1" };

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function AttestForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div><label>Party id (embedded in checkpoint)</label><input value={v.party} onChange={(e) => upd("party", e.target.value)} disabled={disabled || busy} /></div>
      <div className="grid-2">
        <div><label>History growth (records / sec)</label><input type="number" step="any" min={0.1} max={20} value={v.historyGrowthPerSec} onChange={(e) => upd("historyGrowthPerSec", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Checkpoint interval (seconds; 0 = manual)</label><input type="number" min={0} value={v.intervalSecs} onChange={(e) => upd("intervalSecs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Webhook failure rate (0..1)</label><input type="number" step="any" min={0} max={1} value={v.webhookFailureRate} onChange={(e) => upd("webhookFailureRate", e.target.value)} disabled={disabled || busy} /></div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start attesting"}</button>
    </form>
  );
}
