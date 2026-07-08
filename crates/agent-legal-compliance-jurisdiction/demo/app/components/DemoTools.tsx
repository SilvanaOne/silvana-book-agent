"use client";

import type { LegalState } from "@/lib/legal-engine";

type Props = Readonly<{ agent: LegalState | null }>;

export function DemoTools({ agent }: Props) {
  if (!agent || agent.status !== "running") return <div className="muted">Start evaluating to see violations flow.</div>;
  return (
    <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
      Every settlement is mapped through <span className="mono">party_jurisdictions</span> to a jurisdiction, then evaluated against that jurisdiction&apos;s rules. Try:
      <ul style={{ paddingLeft: 18, marginTop: 8 }}>
        <li>Add <span className="mono">blocks=</span> to a jurisdiction — its trades with the blocked jurisdictions become violations.</li>
        <li>Restrict <span className="mono">allowed=</span> to a single market — trades in other markets fail as <span className="mono">market_not_allowed</span>.</li>
        <li>Lower <span className="mono">max_notional=</span> — large trades hit the per-trade cap.</li>
      </ul>
    </div>
  );
}
