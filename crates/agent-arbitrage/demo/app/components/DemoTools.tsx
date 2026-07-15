"use client";

import { useState } from "react";
import type { ArbitrageState } from "@/lib/arbitrage-engine";

type Props = Readonly<{
  arbitrage: ArbitrageState | null;
  onNudge: (to: number) => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
}>;

export function DemoTools({ arbitrage, onNudge, onWalk }: Props) {
  const [manual, setManual] = useState("");
  const [drift, setDrift] = useState("0");
  const [vol, setVol] = useState("6");

  if (!arbitrage || arbitrage.status !== "scanning") {
    return <div className="muted">Start the scanner to enable demo tools.</div>;
  }

  const bps = arbitrage.pairBps[arbitrage.config.focusPair] ?? 0;
  const bump = (delta: number) => Math.max(15, Math.round(bps + delta));

  return (
    <div className="stack">
      <h3>Nudge {arbitrage.config.focusPair} spread (bps)</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onNudge(bump(25))}>+25</button>
        <button className="ghost" onClick={() => onNudge(bump(75))}>+75</button>
        <button className="ghost" onClick={() => onNudge(bump(150))}>+150</button>
        <button className="ghost" onClick={() => onNudge(bump(-25))}>−25</button>
        <button className="ghost" onClick={() => onNudge(bump(-75))}>−75</button>
        <button className="ghost" onClick={() => onNudge(bump(-150))}>−150</button>
      </div>
      <div className="row" style={{ gap: 6 }}>
        <input type="number" step="any" placeholder="Set bps" value={manual} onChange={(e) => setManual(e.target.value)} />
        <button disabled={!manual || !Number.isFinite(Number(manual))} onClick={() => onNudge(Number(manual))}>Set</button>
      </div>

      <h3 style={{ marginTop: 14 }}>Random walk params</h3>
      <div className="grid-2">
        <div>
          <label>Drift / tick (bps)</label>
          <input type="number" step="any" value={drift} onChange={(e) => setDrift(e.target.value)} onBlur={() => { const n = Number(drift); if (Number.isFinite(n)) onWalk({ driftPerTick: n }); }} />
        </div>
        <div>
          <label>Vol / tick (bps)</label>
          <input type="number" step="any" value={vol} onChange={(e) => setVol(e.target.value)} onBlur={() => { const n = Number(vol); if (Number.isFinite(n) && n >= 0) onWalk({ volPerTick: n }); }} />
        </div>
      </div>
    </div>
  );
}
