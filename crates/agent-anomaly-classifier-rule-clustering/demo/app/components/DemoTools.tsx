"use client";

import type { ClassifierState } from "@/lib/classifier-engine";

type Props = Readonly<{ agent: ClassifierState | null }>;

export function DemoTools({ agent }: Props) {
  if (!agent || agent.status !== "running") return <div className="muted">Start classifying to see live buckets fill up.</div>;
  return (
    <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
      Rules (per record):
      <ul style={{ paddingLeft: 18, marginTop: 8 }}>
        <li><span className="mono">stuck_settlement</span> events pass through as-is.</li>
        <li>≥ 1 <span className="mono">layer_cluster</span> AND (rapid + burst) ≥ threshold in window → <span className="mono">spoofing</span>.</li>
        <li>(rapid + burst) ≥ 2× threshold → <span className="mono">spoofing</span> (even without layering).</li>
        <li>Window size ≥ 2× threshold AND no layering → <span className="mono">wash_trading</span>.</li>
        <li>Otherwise → <span className="mono">normal_volatility</span>.</li>
      </ul>
      Try lowering <span className="mono">cluster_threshold</span> — the same stream re-buckets aggressively into spoofing/wash.
    </div>
  );
}
