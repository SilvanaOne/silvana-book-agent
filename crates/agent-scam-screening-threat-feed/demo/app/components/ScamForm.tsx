"use client";

import { useState } from "react";

export type FormValues = { categories: string; parties: string; markets: string; eventRatePerSec: string; refreshSecs: string };

const DEFAULT_CATEGORIES = `category=scam_db severity=warn parties=party::scammer,party::phisher
category=sanctions severity=critical parties=party::ofac_hit
category=honeypot severity=info parties=party::honeypot_a`;

const DEFAULTS: FormValues = {
  categories: DEFAULT_CATEGORIES,
  parties: "party::alice,party::bob,party::scammer,party::phisher,party::ofac_hit,party::honeypot_a,party::clean_d",
  markets: "CC-USDC,BTC-USD",
  eventRatePerSec: "3",
  refreshSecs: "20",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function ScamForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div>
        <label>Categories (one per line: <span className="mono">category= severity= parties=</span>)</label>
        <textarea value={v.categories} onChange={(e) => upd("categories", e.target.value)} disabled={disabled || busy} rows={5}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }} />
      </div>
      <div><label>Party pool</label><input value={v.parties} onChange={(e) => upd("parties", e.target.value)} disabled={disabled || busy} /></div>
      <div className="grid-2">
        <div><label>Market pool</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Event rate / sec</label><input type="number" step="any" min={0.1} max={50} value={v.eventRatePerSec} onChange={(e) => upd("eventRatePerSec", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div><label>Feed refresh (seconds)</label><input type="number" min={1} max={3600} value={v.refreshSecs} onChange={(e) => upd("refreshSecs", e.target.value)} disabled={disabled || busy} /></div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start screening"}</button>
    </form>
  );
}
