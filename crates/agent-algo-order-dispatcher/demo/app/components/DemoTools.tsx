"use client";

import { useState } from "react";
import type { AlgoState } from "@/lib/algo-engine";

type Props = Readonly<{
  agent: AlgoState | null;
  onJump: (to: number) => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
}>;

export function DemoTools({ agent, onJump, onWalk }: Props) {
  const [manual, setManual] = useState("");
  const [vol, setVol] = useState("0.4");

  if (!agent || agent.status !== "running") return <div className="muted">Start dispatcher to enable demo tools.</div>;
  const price = agent.currentPrice;
  const nudge = (mult: number) => Number((price * mult).toFixed(8));

  return (
    <div className="stack">
      <h3>Nudge mid</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onJump(nudge(1.01))}>+1%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.05))}>+5%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.99))}>−1%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.95))}>−5%</button>
      </div>
      <div className="muted" style={{ fontSize: 12.5 }}>
        Push the mid outside the iceberg step&apos;s <span className="mono">price</span> to freeze its fills — watch its bar stall while TWAP keeps ticking through slices.
      </div>
      <div className="row" style={{ gap: 6 }}>
        <input type="number" step="any" placeholder="Manual price" value={manual} onChange={(e) => setManual(e.target.value)} />
        <button disabled={!manual || !Number.isFinite(Number(manual))} onClick={() => { onJump(Number(manual)); setManual(""); }}>Set</button>
      </div>

      <h3 style={{ marginTop: 14 }}>Mid walk</h3>
      <div>
        <label>Vol / tick (%)</label>
        <input type="number" step="any" value={vol} onChange={(e) => setVol(e.target.value)} onBlur={() => { const n = Number(vol); if (Number.isFinite(n) && n >= 0) onWalk({ volPerTick: n / 100 }); }} />
      </div>
    </div>
  );
}
