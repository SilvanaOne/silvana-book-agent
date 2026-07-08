"use client";

import type { TestRunState } from "@/lib/testrun-engine";

type Props = Readonly<{ agent: TestRunState | null; onTrigger: () => Promise<void> }>;

export function DemoTools({ agent, onTrigger }: Props) {
  if (!agent) return <div className="muted">Start test-run to enable demo tools.</div>;

  const inFlight = agent.currentRun !== null;
  const nextIn = agent.nextRunAt ? Math.max(0, Math.round((agent.nextRunAt - Date.now()) / 1000)) : null;

  return (
    <div className="stack">
      <h3>Manual controls</h3>
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <button className="primary" disabled={inFlight} onClick={onTrigger}>Trigger run now</button>
      </div>
      <div className="muted" style={{ fontSize: 12.5, marginTop: 4 }}>
        {inFlight
          ? `Run in flight — ${agent.currentRun!.results.filter((r) => r.status !== "pending").length} / ${agent.currentRun!.results.length} checks done.`
          : nextIn !== null
            ? `Next scheduled run in ${nextIn}s.`
            : "One-shot mode. Trigger the next run manually."}
      </div>

      <h3 style={{ marginTop: 14 }}>Failure knob (live)</h3>
      <div className="muted" style={{ fontSize: 12.5 }}>
        Failure rate is set at start time. Restart with a higher rate (e.g. 0.30) to see reds. Restart with 0 to see all-green.
      </div>
    </div>
  );
}
