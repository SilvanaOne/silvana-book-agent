"use client";

import { useState } from "react";
import type { TreasuryState } from "@/lib/treasury-engine";

type Props = Readonly<{
  agent: TreasuryState | null;
  onJump: (to: number) => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
}>;

export function DemoTools({ agent, onJump, onWalk }: Props) {
  const [manual, setManual] = useState("");
  const [vol, setVol] = useState("0.5");

  if (!agent || agent.status !== "running") return <div className="muted">Start monitoring to see routing decisions.</div>;

  const first = Object.keys(agent.prices)[0];
  const price = first ? agent.prices[first] : 1;
  const nudge = (mult: number) => Number((price * mult).toFixed(8));

  return (
    <div className="stack">
      <h3>Shock driver market ({first ?? "—"})</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onJump(nudge(1.10))}>+10%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.25))}>+25%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.90))}>−10%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.75))}>−25%</button>
      </div>
      <div className="muted" style={{ fontSize: 12.5 }}>
        A big shock will force large rebalance legs. Watch how legs above the <span className="mono">approval</span> threshold get routed to the queue instead of firing directly, and how the 24h budget bar climbs.
      </div>
      <div className="row" style={{ gap: 6 }}>
        <input type="number" step="any" placeholder="Manual price" value={manual} onChange={(e) => setManual(e.target.value)} />
        <button disabled={!manual || !Number.isFinite(Number(manual))} onClick={() => { onJump(Number(manual)); setManual(""); }}>Set</button>
      </div>

      <h3 style={{ marginTop: 14 }}>Ambient walk</h3>
      <div><label>Vol / tick (%)</label><input type="number" step="any" value={vol} onChange={(e) => setVol(e.target.value)} onBlur={() => { const n = Number(vol); if (Number.isFinite(n) && n >= 0) onWalk({ volPerTick: n / 100 }); }} /></div>
    </div>
  );
}
