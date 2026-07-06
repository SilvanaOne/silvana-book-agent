"use client";

import type { AnomalyState } from "@/lib/anomaly-engine";

type Props = Readonly<{ agent: AnomalyState | null; onReload: () => Promise<void>; onReScan: () => Promise<void> }>;

export function DemoTools({ agent, onReload, onReScan }: Props) {
  if (!agent) return <div className="muted">Load a history to enable demo tools.</div>;
  return (
    <div className="stack">
      <h3>Batch ops</h3>
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <button className="ghost" onClick={onReload}>Regenerate history</button>
        <button className="primary" onClick={onReScan}>Re-scan</button>
      </div>
      <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
        Tighten thresholds (lower <span className="mono">rapid_cancel_window_ms</span>, or lower <span className="mono">layer_threshold</span>) and click Re-scan — you&apos;ll see the anomaly count climb on the same history.
      </div>
    </div>
  );
}
