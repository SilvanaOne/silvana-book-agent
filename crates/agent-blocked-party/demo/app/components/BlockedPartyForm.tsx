"use client";

import { useState } from "react";

export type FormValues = {
  blocklist: string;                     // newline-separated in the textarea
  reloadSecs: string;
  settlementArrivalPerTick: string;
  blockedPartyProbability: string;
  startingPrice: string;
};

const DEFAULT_BLOCKLIST = [
  "party-scammer-1",
  "party-fraud-2",
  "party-blocked-3",
].join("\n");

const DEFAULTS: FormValues = {
  blocklist: DEFAULT_BLOCKLIST,
  reloadSecs: "30",
  settlementArrivalPerTick: "0.3",
  blockedPartyProbability: "0.08",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function BlockedPartyForm({ disabled, onStart }: Props) {
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
        <label>Blocklist (one party id per line, <span className="mono">#</span> for comments)</label>
        <textarea
          value={v.blocklist}
          onChange={(e) => upd("blocklist", e.target.value)}
          disabled={disabled || busy}
          rows={5}
          spellCheck={false}
          style={{
            width: "100%",
            fontFamily: "ui-monospace, monospace",
            fontSize: 13,
            background: "var(--bg-card)",
            color: "var(--text)",
            border: "1px solid var(--border)",
            borderRadius: 6,
            padding: 8,
            resize: "vertical",
          }}
        />
      </div>
      <div className="grid-2">
        <div><label>Reload interval (secs)</label><input type="number" step="1" value={v.reloadSecs} onChange={(e) => upd("reloadSecs", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Settlement arrival λ / tick</label><input type="number" step="any" value={v.settlementArrivalPerTick} onChange={(e) => upd("settlementArrivalPerTick", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Blocked-party probability (0..1)</label><input type="number" step="any" value={v.blockedPartyProbability} onChange={(e) => upd("blockedPartyProbability", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Blocked Party"}</button>
    </form>
  );
}
