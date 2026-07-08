"use client";

import { useState } from "react";

export type FormValues = {
  markets: string;
  mids: string;
  minConfidence: string;
  model: string;
  autoPromptEverySec: string;
  prompts: string;
};

const DEFAULT_PROMPTS = `I think CC is going to pump within the hour, strong confidence
BTC looks bearish, news event might crash it
maybe eth is going up, low confidence
CC bullish rally starting
Uncertain about the direction`;

const DEFAULTS: FormValues = {
  markets: "CC-USDC,BTC-USD",
  mids: "1.02,60000",
  minConfidence: "0.6",
  model: "silvana-mock-v1",
  autoPromptEverySec: "5",
  prompts: DEFAULT_PROMPTS,
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function AiSignalForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>Markets (BASE-QUOTE)</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Mids (comma; one per market)</label><input value={v.mids} onChange={(e) => upd("mids", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Min confidence [0..1]</label><input type="number" step="any" min={0} max={1} value={v.minConfidence} onChange={(e) => upd("minConfidence", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Model name</label><input value={v.model} onChange={(e) => upd("model", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div>
        <label>Auto-emit interval (seconds; 0 = manual only)</label>
        <input type="number" min={0} value={v.autoPromptEverySec} onChange={(e) => upd("autoPromptEverySec", e.target.value)} disabled={disabled || busy} />
      </div>
      <div>
        <label>Prompt pool (one per line — used by auto-emit)</label>
        <textarea value={v.prompts} onChange={(e) => upd("prompts", e.target.value)} disabled={disabled || busy} rows={6}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }} />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start predicting"}</button>
    </form>
  );
}
