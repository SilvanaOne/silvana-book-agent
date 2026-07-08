"use client";

import type { AttestState } from "@/lib/attest-engine";

type Props = Readonly<{ agent: AttestState | null; onPublish: () => Promise<void> }>;

export function DemoTools({ agent, onPublish }: Props) {
  if (!agent || agent.status !== "running") return <div className="muted">Start attesting to enable demo tools.</div>;
  return (
    <div className="stack">
      <h3>Manual publish</h3>
      <button className="primary" onClick={onPublish}>Publish checkpoint now</button>
      <div className="muted" style={{ fontSize: 12.5 }}>
        Any single checkpoint proves that all earlier records existed at that ts. Try running with a high failure rate — you&apos;ll see FAIL entries in the event log for webhook-delivery drops (a real deployment would retry).
      </div>
    </div>
  );
}
