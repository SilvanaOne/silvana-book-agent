"use client";

import { useState } from "react";

export type FormValues = {
  ordersJsonl: string;
  submitRatePerTick: string;
  abortOnError: boolean;
  failureRatePerTick: string;
  startingPrice: string;
};

const SAMPLE_JSONL = `{"market":"CC-USDC","side":"buy","quantity":"1","price":"0.148","ref":"batch-bid-1"}
{"market":"CC-USDC","side":"buy","quantity":"2","price":"0.146","ref":"batch-bid-2"}
{"market":"CC-USDC","side":"buy","quantity":"3","price":"0.144","ref":"batch-bid-3"}
{"market":"CC-USDC","side":"sell","quantity":"1","price":"0.152","ref":"batch-offer-1"}
{"market":"CC-USDC","side":"sell","quantity":"2","price":"0.154","ref":"batch-offer-2"}
{"market":"CC-USDC","side":"sell","quantity":"3","price":"0.156","ref":"batch-offer-3"}
{"market":"CC-USDC","side":"buy","quantity":"1.5","price":"0.150","ref":"batch-bid-4"}
{"market":"CC-USDC","side":"sell","quantity":"1.5","price":"0.150","ref":"batch-offer-4"}`;

const DEFAULTS: FormValues = {
  ordersJsonl: SAMPLE_JSONL,
  submitRatePerTick: "3",
  abortOnError: false,
  failureRatePerTick: "0.05",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function BatchOrdersForm({ disabled, onStart }: Props) {
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
      <div>
        <label>Orders (JSONL) — one order per line</label>
        <textarea
          value={v.ordersJsonl}
          onChange={(e) => upd("ordersJsonl", e.target.value)}
          disabled={disabled || busy}
          rows={9}
          spellCheck={false}
          style={{ fontFamily: "ui-monospace, monospace", fontSize: 12, width: "100%", resize: "vertical" }}
        />
        <div className="faint" style={{ fontSize: 12, marginTop: 4 }}>
          {`{"market":"CC-USDC","side":"buy"|"sell","quantity":"...","price":"...","ref":"..."}`} · # comments and blank lines ignored
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Submit rate / tick</label>
          <input
            type="number"
            step="1"
            min="1"
            value={v.submitRatePerTick}
            onChange={(e) => upd("submitRatePerTick", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
        <div>
          <label>Failure rate / tick (0..1)</label>
          <input
            type="number"
            step="any"
            min="0"
            max="1"
            value={v.failureRatePerTick}
            onChange={(e) => upd("failureRatePerTick", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Starting price (mid seed)</label>
          <input
            type="number"
            step="any"
            value={v.startingPrice}
            onChange={(e) => upd("startingPrice", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
        <div style={{ display: "flex", alignItems: "flex-end" }}>
          <label style={{ display: "flex", alignItems: "center", gap: 8, cursor: disabled || busy ? "default" : "pointer" }}>
            <input
              type="checkbox"
              checked={v.abortOnError}
              onChange={(e) => upd("abortOnError", e.target.checked)}
              disabled={disabled || busy}
            />
            <span>Abort on first error</span>
          </label>
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>
        {busy ? "Submitting…" : "Submit batch"}
      </button>
    </form>
  );
}
