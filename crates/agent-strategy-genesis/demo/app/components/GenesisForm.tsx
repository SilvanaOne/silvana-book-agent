"use client";

import { useState } from "react";

type Props = Readonly<{ onCompile: (spec: string) => Promise<void>; onReset: () => Promise<void> }>;

const PRESETS = [
  "buy 100 CC-USDC over 1 hour using TWAP",
  "sell 50 BTC-USD with iceberg visible 5 price 60000",
  "buy 200 CC-USDC minimize slippage bps 25 chunk 10 depth 30",
  "sell 25 ETH-USDC over 30 minutes",
  "buy 5 BTC-USD iceberg visible 0.5 price 58500 poll 2",
];

export function GenesisForm({ onCompile, onReset }: Props) {
  const [spec, setSpec] = useState(PRESETS[0]);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (spec.trim().length === 0) return;
    setBusy(true); setErr(null);
    try { await onCompile(spec.trim()); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); }
  }

  return (
    <form onSubmit={submit} className="stack">
      <div>
        <label>Natural-language spec</label>
        <textarea value={spec} onChange={(e) => setSpec(e.target.value)} rows={4}
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12.5, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }} />
      </div>
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        {PRESETS.map((p, i) => (
          <button key={i} type="button" className="ghost" onClick={() => setSpec(p)}>{i + 1}</button>
        ))}
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <div className="row" style={{ gap: 8 }}>
        <button type="submit" className="primary" disabled={busy}>{busy ? "Compiling…" : "Compile"}</button>
        <button type="button" className="ghost" onClick={() => onReset()}>Reset history</button>
      </div>
    </form>
  );
}
