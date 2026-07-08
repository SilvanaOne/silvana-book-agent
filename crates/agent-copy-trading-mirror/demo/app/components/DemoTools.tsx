"use client";

import type { CopyState } from "@/lib/copy-engine";

type Props = Readonly<{ agent: CopyState | null }>;

export function DemoTools({ agent }: Props) {
  if (!agent || agent.status !== "running") return <div className="muted">Start mirroring to enable demo tools.</div>;
  return (
    <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
      Every ~{(1 / agent.config.leaderRatePerSec).toFixed(2)}s the engine generates a synthetic leader order on a random market from the pool. Every mirror is BID/OFFER/price mirrored from the leader, scaled by <span className="mono">scale</span>. Try:
      <ul style={{ paddingLeft: 18, marginTop: 8 }}>
        <li>Drop a market from <span className="mono">markets whitelist</span> — leader trades on it become <span className="mono">market_filter</span> refusals.</li>
        <li>Lower <span className="mono">max_mirror_notional</span> — large leader trades still spawn but their mirrors get rejected on cap.</li>
        <li>Bump <span className="mono">scale</span> from 30% to 100% — follower net position climbs to match leader.</li>
      </ul>
    </div>
  );
}
