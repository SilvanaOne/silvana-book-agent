"use client";

import type { ExplainState } from "@/lib/explain-engine";

type Props = Readonly<{ agent: ExplainState | null; onReRun: () => Promise<void> }>;

export function DemoTools({ agent, onReRun }: Props) {
  if (!agent) return <div className="muted">Load a history to enable tools.</div>;
  return (
    <div className="stack">
      <h3>Batch ops</h3>
      <button className="primary" onClick={onReRun}>Regenerate + re-explain</button>
      <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
        The mock rationale generator is deterministic per record — same features (size class, direction, market) → same wording. Regenerate to draw a fresh synthetic history + freshly rendered text.
      </div>
    </div>
  );
}
