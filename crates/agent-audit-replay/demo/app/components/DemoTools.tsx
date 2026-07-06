"use client";

import type { ReplayState } from "@/lib/replay-engine";

type Props = Readonly<{ agent: ReplayState | null; onReload: () => Promise<void>; onReRun: () => Promise<void> }>;

export function DemoTools({ agent, onReload, onReRun }: Props) {
  if (!agent) return <div className="muted">Load a history to enable demo tools.</div>;
  return (
    <div className="stack">
      <h3>Batch ops</h3>
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <button className="ghost" onClick={onReload}>Regenerate history</button>
        <button className="primary" onClick={onReRun}>Re-run rules</button>
      </div>
      <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
        Tighten <span className="mono">max_notional_per_order</span> or add markets to <span className="mono">blocked_markets</span> and click <span className="mono">Re-run rules</span> to see the reject rate change on the same synthetic log.
      </div>
    </div>
  );
}
