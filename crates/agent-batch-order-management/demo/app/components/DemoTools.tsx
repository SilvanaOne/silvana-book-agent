"use client";

import { useState } from "react";
import type { DriftAnalysis } from "@/lib/rebalance-engine";

type Props = Readonly<{
  analysis: DriftAnalysis;
  volPerTick: number;
  onDrift: (assetSymbol: string, driftWeight: number) => Promise<void>;
  onThreshold: (bps: number) => Promise<void>;
  onWalk: (volPerTick: number) => Promise<void>;
  onReset: () => Promise<void>;
}>;

export function DemoTools({ analysis, volPerTick, onDrift, onThreshold, onWalk, onReset }: Props) {
  const nonQuote = analysis.rows.filter((r) => !r.isQuote);
  const [asset, setAsset] = useState(nonQuote[0]?.assetSymbol ?? "");
  const [threshold, setThreshold] = useState(String(analysis.thresholdBps));

  const activeAsset = nonQuote.some((r) => r.assetSymbol === asset) ? asset : nonQuote[0]?.assetSymbol ?? "";

  return (
    <div className="stack">
      <h3>Imitate drift</h3>
      <div className="row" style={{ gap: 6 }}>
        <select value={activeAsset} onChange={(e) => setAsset(e.target.value)} style={{ maxWidth: 130 }}>
          {nonQuote.map((r) => (
            <option key={r.assetSymbol} value={r.assetSymbol}>{r.assetSymbol}</option>
          ))}
        </select>
        <button className="ghost" onClick={() => onDrift(activeAsset, 0.1)}>+10pp</button>
        <button className="ghost" onClick={() => onDrift(activeAsset, 0.2)}>+20pp</button>
        <button className="ghost" onClick={() => onDrift(activeAsset, -0.1)}>−10pp</button>
      </div>
      <div className="muted" style={{ fontSize: 11 }}>
        Shifts the asset off its target weight; the remainder is redistributed pro-rata across the others (NAV held constant).
      </div>

      <h3 style={{ marginTop: 12 }}>Tune</h3>
      <div className="grid-2">
        <div>
          <label>Threshold (bps)</label>
          <input
            type="number"
            step="1"
            value={threshold}
            onChange={(e) => setThreshold(e.target.value)}
            onBlur={() => { const n = Number(threshold); if (Number.isFinite(n) && n >= 0) onThreshold(Math.round(n)); }}
          />
        </div>
        <div>
          <label>Price walk vol/tick (%)</label>
          <input
            type="number"
            step="any"
            defaultValue={(volPerTick * 100).toString()}
            onBlur={(e) => { const n = Number(e.target.value); if (Number.isFinite(n) && n >= 0) onWalk(n / 100); }}
          />
        </div>
      </div>
      <div className="muted" style={{ fontSize: 11 }}>
        Vol {'>'} 0 makes non-quote prices wander each tick so drift breathes between rebalances. USDC stays pinned to 1.
      </div>

      <div className="row" style={{ marginTop: 8 }}>
        <button className="danger" onClick={onReset}>Reset to seed</button>
      </div>
    </div>
  );
}
