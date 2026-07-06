"use client";

import { useState } from "react";
import type { InvRiskState } from "@/lib/invrisk-engine";

type Props = Readonly<{
  agent: InvRiskState | null;
  onNudge: (delta: number) => Promise<void>;
  onToggleAutoHedge: () => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
}>;

export function DemoTools({ agent, onNudge, onToggleAutoHedge, onWalk }: Props) {
  const [manual, setManual] = useState("");
  const [vol, setVol] = useState("0.5");

  if (!agent || agent.status !== "running") return <div className="muted">Start monitoring to enable demo tools.</div>;

  const soft = agent.config.softTolerance;
  const hard = agent.config.hardTolerance;

  return (
    <div className="stack">
      <h3>Auto-hedge toggle</h3>
      <div className="row" style={{ gap: 6 }}>
        <button className={agent.config.autoHedge ? "danger" : "primary"} onClick={onToggleAutoHedge}>
          auto_hedge: {agent.config.autoHedge ? "ON — click to disable" : "OFF — click to enable"}
        </button>
      </div>

      <h3 style={{ marginTop: 14 }}>Push balance out of band</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onNudge(soft * 1.2)}>+soft</button>
        <button className="ghost" onClick={() => onNudge(hard * 1.2)}>+hard</button>
        <button className="ghost" onClick={() => onNudge(hard * 2)}>+2× hard</button>
        <button className="ghost" onClick={() => onNudge(-soft * 1.2)}>−soft</button>
        <button className="ghost" onClick={() => onNudge(-hard * 1.2)}>−hard</button>
        <button className="ghost" onClick={() => onNudge(-hard * 2)}>−2× hard</button>
      </div>
      <div className="row" style={{ gap: 6 }}>
        <input type="number" step="any" placeholder="Manual delta" value={manual} onChange={(e) => setManual(e.target.value)} />
        <button disabled={!manual || !Number.isFinite(Number(manual))} onClick={() => { onNudge(Number(manual)); setManual(""); }}>Nudge</button>
      </div>

      <h3 style={{ marginTop: 14 }}>Mid-price walk</h3>
      <div>
        <label>Vol / tick (%)</label>
        <input type="number" step="any" value={vol} onChange={(e) => setVol(e.target.value)} onBlur={() => { const n = Number(vol); if (Number.isFinite(n) && n >= 0) onWalk({ volPerTick: n / 100 }); }} />
      </div>
    </div>
  );
}
