"use client";

import { useState } from "react";

export type FormValues = {
  handlersText: string;
  market: string;
  eventArrivalPerTick: string;
  commandDurationMs: string;
  commandFailureRate: string;
  startingPrice: string;
};

const DEFAULT_HANDLERS = [
  "settled:./notify-settled.sh",
  "failed:./page-oncall.sh",
  "cancelled:echo cancelled",
  "order-filled:./record-fill.sh",
].join("\n");

const DEFAULTS: FormValues = {
  handlersText: DEFAULT_HANDLERS,
  market: "",
  eventArrivalPerTick: "0.3",
  commandDurationMs: "200",
  commandFailureRate: "0.02",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function WitnessesForm({ disabled, onStart }: Props) {
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
        <label>Handlers (one per line: <span className="mono">kind:command</span>)</label>
        <textarea
          value={v.handlersText}
          onChange={(e) => upd("handlersText", e.target.value)}
          disabled={disabled || busy}
          rows={5}
          className="mono"
          style={{ width: "100%", fontSize: 12, padding: 8, background: "var(--bg-input, #0d0d13)", color: "inherit", border: "1px solid #2a2a35", borderRadius: 4, resize: "vertical" }}
          placeholder="settled:./notify-settled.sh"
        />
        <div className="muted" style={{ fontSize: 11, marginTop: 4 }}>
          Kinds: settled · failed · cancelled · proposal · order-filled · order-cancelled
        </div>
      </div>
      <div className="grid-2">
        <div><label>Market filter (optional)</label><input value={v.market} placeholder="e.g. CC-USDC (blank = all)" onChange={(e) => upd("market", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Starting price (mid seed)</label><input type="number" step="any" value={v.startingPrice} onChange={(e) => upd("startingPrice", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Event arrival / tick (0..1)</label><input type="number" step="any" value={v.eventArrivalPerTick} onChange={(e) => upd("eventArrivalPerTick", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Command duration (ms, avg)</label><input type="number" step="1" value={v.commandDurationMs} onChange={(e) => upd("commandDurationMs", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Command failure rate (0..1)</label><input type="number" step="any" value={v.commandFailureRate} onChange={(e) => upd("commandFailureRate", e.target.value)} disabled={disabled || busy} /></div>
        <div />
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Witnesses"}</button>
    </form>
  );
}
