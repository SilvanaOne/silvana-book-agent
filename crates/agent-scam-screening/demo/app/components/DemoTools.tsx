"use client";

import type { ScamState } from "@/lib/scam-engine";

type Props = Readonly<{ agent: ScamState | null }>;

export function DemoTools({ agent }: Props) {
  if (!agent || agent.status !== "running") return <div className="muted">Start screening to see the threat feed in action.</div>;
  return (
    <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
      Each simulated settlement is checked against every category. Watch:
      <ul style={{ paddingLeft: 18, marginTop: 8 }}>
        <li>Verdict badges — <span className="mono">CRITICAL</span> (red), <span className="mono">WARN</span> (orange), <span className="mono">INFO</span> (yellow), <span className="mono">CLEAN</span> (green).</li>
        <li>Feed refresh (every <span className="mono">{agent.config.refreshSecs}s</span>) — occasionally injects a new pool party into a lower-severity category, so previously-clean parties can suddenly start matching.</li>
        <li>Add parties from the pool into a <span className="mono">critical</span> category to spike criticals fast.</li>
      </ul>
    </div>
  );
}
