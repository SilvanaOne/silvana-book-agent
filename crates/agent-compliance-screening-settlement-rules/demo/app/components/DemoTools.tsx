"use client";

import type { ComplianceState } from "@/lib/compliance-engine";

type Props = Readonly<{ agent: ComplianceState | null }>;

export function DemoTools({ agent }: Props) {
  if (!agent || agent.status !== "running") return <div className="muted">Start screening to see settlement flow.</div>;
  return (
    <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
      Every ~{(1 / agent.config.eventRatePerSec).toFixed(2)}s the engine generates one synthetic settlement pairing two random parties from the pool on one of the configured markets, then evaluates it against the policy. Try:
      <ul style={{ paddingLeft: 18, marginTop: 8 }}>
        <li>Add <span className="mono">blocked_pair=</span> lines to force rejects.</li>
        <li>Tighten <span className="mono">party_cap=</span> maxes — rolling window fills up and starts rejecting.</li>
        <li>Set a market filter — only settlements on that market are screened.</li>
      </ul>
    </div>
  );
}
