"use client";

import { useState } from "react";
import type { PairsTradingState } from "@/lib/pairstrading-engine";

type Props = Readonly<{
  pairstrading: PairsTradingState | null;
  onJump: (to: number) => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
}>;

export function DemoTools({ pairstrading, onJump, onWalk }: Props) {
  const [manual, setManual] = useState("");
  const [drift, setDrift] = useState("0");
  const [vol, setVol] = useState("0.8");

  if (!pairstrading || pairstrading.status !== "monitoring") return <div className="muted">Start Pairs Trading to enable demo tools.</div>;

  // Nudge acts on leg A price (master walk). Leg B keeps drifting independently.
  const priceA = pairstrading.priceA;
  const nudge = (mult: number) => Number((priceA * mult).toFixed(8));

  return (
    <div className="stack">
      <h3>Nudge leg-A price</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onJump(nudge(1.005))}>+0.5%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.02))}>+2%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.05))}>+5%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.995))}>−0.5%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.98))}>−2%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.95))}>−5%</button>
      </div>
      <div className="row" style={{ gap: 6 }}>
        <input type="number" step="any" placeholder="Manual price A" value={manual} onChange={(e) => setManual(e.target.value)} />
        <button disabled={!manual || !Number.isFinite(Number(manual))} onClick={() => onJump(Number(manual))}>Set</button>
      </div>

      <h3 style={{ marginTop: 14 }}>Random walk params (leg A)</h3>
      <div className="grid-2">
        <div>
          <label>Drift / tick</label>
          <input type="number" step="any" value={drift} onChange={(e) => setDrift(e.target.value)} onBlur={() => { const n = Number(drift); if (Number.isFinite(n)) onWalk({ driftPerTick: n }); }} />
        </div>
        <div>
          <label>Vol / tick (%)</label>
          <input type="number" step="any" value={vol} onChange={(e) => setVol(e.target.value)} onBlur={() => { const n = Number(vol); if (Number.isFinite(n) && n >= 0) onWalk({ volPerTick: n / 100 }); }} />
        </div>
      </div>
    </div>
  );
}
