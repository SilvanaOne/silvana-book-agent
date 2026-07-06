"use client";

import { useState } from "react";
import type { SmartAllocState } from "@/lib/smartalloc-engine";

type Props = Readonly<{
  agent: SmartAllocState | null;
  onJump: (to: number) => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
}>;

export function DemoTools({ agent, onJump, onWalk }: Props) {
  const [manual, setManual] = useState("");
  const [vol, setVol] = useState("0.5");

  if (!agent || agent.status !== "running") return <div className="muted">Start allocation to enable demo tools.</div>;

  const firstMarket = Object.keys(agent.prices)[0];
  const price = firstMarket ? agent.prices[firstMarket] : 1;
  const nudge = (mult: number) => Number((price * mult).toFixed(8));

  return (
    <div className="stack">
      <h3>Jolt driver market ({firstMarket ?? "—"})</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onJump(nudge(1.05))}>+5%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.15))}>+15%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.30))}>+30%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.95))}>−5%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.85))}>−15%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.70))}>−30%</button>
      </div>
      <div className="muted" style={{ fontSize: 12.5 }}>
        Shocking the driver market unbalances its bucket — watch weights breach the ±threshold band and rebalance orders fire.
      </div>
      <div className="row" style={{ gap: 6 }}>
        <input type="number" step="any" placeholder="Manual price" value={manual} onChange={(e) => setManual(e.target.value)} />
        <button disabled={!manual || !Number.isFinite(Number(manual))} onClick={() => { onJump(Number(manual)); setManual(""); }}>Set</button>
      </div>

      <h3 style={{ marginTop: 14 }}>Ambient price walk</h3>
      <div>
        <label>Vol / tick (%)</label>
        <input type="number" step="any" value={vol} onChange={(e) => setVol(e.target.value)} onBlur={() => { const n = Number(vol); if (Number.isFinite(n) && n >= 0) onWalk({ volPerTick: n / 100 }); }} />
      </div>
    </div>
  );
}
