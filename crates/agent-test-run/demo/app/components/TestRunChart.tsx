"use client";

import type { TestRunState, CheckResult } from "@/lib/testrun-engine";

type Props = Readonly<{ agent: TestRunState | null }>;

const W = 720, H = 340;

export function TestRunChart({ agent }: Props) {
  if (!agent) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start test-run to see per-check pass/fail + latencies.</div>;
  }

  const run = agent.currentRun ?? agent.lastRun;
  if (!run) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Waiting for first run…</div>;
  }

  const rowH = Math.min(36, (H - 60) / run.results.length);
  const maxLatency = Math.max(50, ...run.results.filter((r) => r.status === "pass" || r.status === "fail").map((r) => r.latencyMs), 1);
  const barMaxW = W - 260;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />
      <text x={10} y={22} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        run #{run.seq} · pass {run.passed} · fail {run.failed} · skip {run.skipped} · {run.finishedAt ? run.totalMs + " ms" : "in flight…"}
      </text>

      {run.results.map((r: CheckResult, i: number) => {
        const y = 40 + i * rowH;
        const stateColor = r.status === "pass" ? "var(--pos)" : r.status === "fail" ? "var(--neg)" : r.status === "skipped" ? "var(--text-faint)" : "var(--accent)";
        const stateLabel = r.status === "pass" ? "OK  " : r.status === "fail" ? "FAIL" : r.status === "skipped" ? "SKIP" : "…   ";
        const barW = r.status === "pass" || r.status === "fail" ? (r.latencyMs / maxLatency) * barMaxW : 0;
        return (
          <g key={r.id}>
            {/* status pill */}
            <rect x={10} y={y + 4} width={44} height={rowH - 8} rx={4} fill="var(--bg-card)" stroke={stateColor} strokeWidth={1.2} />
            <text x={32} y={y + rowH / 2 + 4} fill={stateColor} fontSize={11} fontFamily="ui-monospace, monospace" textAnchor="middle">{stateLabel.trim()}</text>

            {/* check id */}
            <text x={64} y={y + rowH / 2 + 4} fill="var(--text)" fontSize={12} fontFamily="ui-monospace, monospace">{r.id}</text>

            {/* latency bar */}
            <rect x={220} y={y + rowH / 2 - 3} width={barW} height={6} fill={stateColor} opacity={0.7} />
            {(r.status === "pass" || r.status === "fail") && (
              <text x={220 + barW + 6} y={y + rowH / 2 + 4} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{r.latencyMs} ms</text>
            )}

            {/* detail */}
            {r.detail && (
              <text x={220} y={y + rowH - 4} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
                {truncate(r.detail, 60)}
              </text>
            )}
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number): string {
  return s.length <= n ? s : s.slice(0, n - 1) + "…";
}
