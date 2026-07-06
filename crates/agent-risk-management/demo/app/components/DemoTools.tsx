"use client";

import { useState } from "react";
import type { RiskState } from "@/lib/riskmgmt-engine";

type Props = Readonly<{
  agent: RiskState | null;
  onJump: (to: number) => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
  onToggleEnforce: () => Promise<void>;
}>;

export function DemoTools({ agent, onJump, onWalk, onToggleEnforce }: Props) {
  const [manual, setManual] = useState("");
  const [drift, setDrift] = useState("0");
  const [vol, setVol] = useState("0.5");

  if (!agent || agent.status !== "running") return <div className="muted">Start monitoring to enable demo tools.</div>;

  const price = agent.currentPrices[0] ?? 1;
  const nudge = (mult: number) => Number((price * mult).toFixed(8));

  return (
    <div className="stack">
      <h3>Enforce toggle</h3>
      <div className="row" style={{ gap: 6 }}>
        <button className={agent.config.enforce ? "danger" : "primary"} onClick={onToggleEnforce}>
          Enforce: {agent.config.enforce ? "ON — click to disable" : "OFF — click to enable"}
        </button>
      </div>

      <h3 style={{ marginTop: 14 }}>Nudge driver price</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onJump(nudge(1.02))}>+2%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.05))}>+5%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.10))}>+10%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.98))}>−2%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.95))}>−5%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.90))}>−10%</button>
      </div>
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
