"use client";

import { useState } from "react";
import type { PositionState } from "@/lib/tpsl-engine";

type Props = Readonly<{
  position: PositionState | null;
  onJump: (to: number) => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
}>;

export function DemoTools({ position, onJump, onWalk }: Props) {
  const [manual, setManual] = useState("");
  const [drift, setDrift] = useState("0");
  const [vol, setVol] = useState("0.8");

  if (!position || position.status !== "monitoring") {
    return <div className="muted">Start a position to enable demo tools.</div>;
  }

  const price = position.currentPrice;
  const nudge = (mult: number) => Number((price * mult).toFixed(8));
  const tp = position.config.tp;
  const sl = position.currentSl;

  return (
    <div className="stack">
      <h3>Nudge price</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onJump(nudge(1.005))}>+0.5%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.02))}>+2%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.05))}>+5%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.995))}>−0.5%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.98))}>−2%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.95))}>−5%</button>
      </div>
      {tp !== null && (
        <button className="ghost" onClick={() => onJump(Number((tp * 1.0001).toFixed(8)))}>
          Jump to TP ({tp})
        </button>
      )}
      {sl !== null && (
        <button className="ghost" onClick={() => onJump(Number((sl * 0.9999).toFixed(8)))}>
          Jump to SL ({sl.toFixed(6)})
        </button>
      )}
      <div className="row" style={{ gap: 6 }}>
        <input type="number" step="any" placeholder="Manual price" value={manual} onChange={(e) => setManual(e.target.value)} />
        <button disabled={!manual || !Number.isFinite(Number(manual))} onClick={() => onJump(Number(manual))}>Set</button>
      </div>

      <h3 style={{ marginTop: 14 }}>Random walk params</h3>
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
