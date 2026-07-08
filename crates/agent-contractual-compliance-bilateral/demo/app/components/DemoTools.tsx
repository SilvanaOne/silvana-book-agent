"use client";

import type { ContractState } from "@/lib/contract-engine";

type Props = Readonly<{ agent: ContractState | null }>;

export function DemoTools({ agent }: Props) {
  if (!agent || agent.status !== "running") return <div className="muted">Start tracking to see contract obligations.</div>;
  return (
    <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
      Synthetic settlements pair a random counterparty with a random market. Contract entries that share both a counterparty and a market accrue the notional. Try:
      <ul style={{ paddingLeft: 18, marginTop: 8 }}>
        <li>Lower a <span className="mono">min</span> and use a shorter <span className="mono">window_hours</span> to see UNDER breaches early.</li>
        <li>Raise the event rate — contracts with low <span className="mono">max</span> flip to OVER quickly.</li>
        <li>Set <span className="mono">expires=</span> to a past date — the contract shows as EXPIRED (min no longer enforced).</li>
      </ul>
    </div>
  );
}
