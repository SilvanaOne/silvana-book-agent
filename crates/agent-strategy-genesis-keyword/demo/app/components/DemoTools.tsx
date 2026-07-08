"use client";

import type { GenesisState } from "@/lib/genesis-engine";

type Props = Readonly<{ agent: GenesisState | null }>;

export function DemoTools({ agent }: Props) {
  if (!agent || agent.steps.length === 0) return <div className="muted">Compile a spec — try the preset chips.</div>;
  return (
    <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
      Parser is a deterministic keyword matcher (drop-in an LLM behind the same interface in prod).
      Rules:
      <ul style={{ paddingLeft: 18, marginTop: 8 }}>
        <li><span className="mono">iceberg | visible</span> → algorithm=iceberg</li>
        <li><span className="mono">liquid | slippage</span> → algorithm=liquidity-seeking</li>
        <li>otherwise → <span className="mono">twap</span></li>
        <li><span className="mono">sell | offer</span> → side=sell; else buy</li>
        <li><span className="mono">over N hour/min/sec</span> → duration_secs (twap)</li>
        <li><span className="mono">keyword N</span> or <span className="mono">N keyword</span> (visible / price / chunk / bps / poll / depth / slices)</li>
      </ul>
    </div>
  );
}
