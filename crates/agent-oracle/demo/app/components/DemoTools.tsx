"use client";

import type { OracleState } from "@/lib/oracle-engine";

type Props = Readonly<{ oracle: OracleState | null }>;

export function DemoTools({ oracle }: Props) {
  if (!oracle) return <div className="muted">Start Oracle to see the live price feed.</div>;

  return (
    <div className="stack" style={{ fontSize: 13, lineHeight: 1.7 }}>
      <p>
        The oracle polls <span className="mono">GetPrice</span> for every market every{" "}
        <span className="mono">{oracle.config.pollSecs}s</span> and emits one record per market to configured sinks
        (stdout, JSONL, webhook). Source is pinned to <span className="mono">{oracle.config.source}</span>.
      </p>
      <p>
        Prices below are simulated (per-market GBM walk). Orange dots on the chart mark each publish event —
        one per market per <span className="mono">pollSecs</span> boundary.
      </p>
      <div className="kv-row"><span className="k">markets tracked</span><span className="v accent">{oracle.currentPrices.length}</span></div>
      <div className="kv-row"><span className="k">records emitted</span><span className="v accent">{oracle.publishedCount}</span></div>
      <div className="kv-row"><span className="k">buffered records</span><span className="v">{oracle.publishedRecords.length} / 60</span></div>
    </div>
  );
}
