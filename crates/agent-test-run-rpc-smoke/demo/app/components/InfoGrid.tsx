"use client";

import type { TestRunState } from "@/lib/testrun-engine";

type Props = Readonly<{ agent: TestRunState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const last = agent?.lastRun;
  const stateLabel = agent === null ? "idle" : agent.status === "running" ? (agent.currentRun ? "probing" : "waiting") : "stopped";
  const nextIn = agent?.nextRunAt ? Math.max(0, Math.round((agent.nextRunAt - Date.now()) / 1000)) : null;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">test-run</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TARGET</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">party</span><span className="v mono" style={{ fontSize: 11 }}>{truncate(c.partyId, 20)}</span></div>
            <div className="kv-row"><span className="k">endpoint</span><span className="v mono" style={{ fontSize: 11 }}>{truncate(c.endpoint, 20)}</span></div>
            <div className="kv-row"><span className="k">market</span><span className="v accent">{c.market || "(none)"}</span></div>
            <div className="kv-row"><span className="k">interval</span><span className="v">{c.runIntervalSecs === 0 ? "one-shot" : c.runIntervalSecs + "s"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">party</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">endpoint</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LAST RUN</div></div>
        {last ? (
          <>
            <div className="kv-row"><span className="k">seq</span><span className="v">#{last.seq}</span></div>
            <div className="kv-row"><span className="k">pass / fail</span><span className="v"><span className="positive">{last.passed}</span> / <span className={last.failed > 0 ? "negative" : ""}>{last.failed}</span></span></div>
            <div className="kv-row"><span className="k">skipped</span><span className="v">{last.skipped}</span></div>
            <div className="kv-row"><span className="k">total time</span><span className="v">{last.totalMs} ms</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">seq</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">pass / fail</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">skipped</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">total time</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CUMULATIVE</div></div>
        <div className="kv-row"><span className="k">runs completed</span><span className="v">{agent?.runsCompleted ?? 0}</span></div>
        <div className="kv-row"><span className="k">total pass</span><span className="v positive">{agent?.totalPassed ?? 0}</span></div>
        <div className="kv-row"><span className="k">total fail</span><span className={`v ${(agent?.totalFailed ?? 0) > 0 ? "negative" : ""}`}>{agent?.totalFailed ?? 0}</span></div>
        <div className="kv-row"><span className="k">total skip</span><span className="v">{agent?.totalSkipped ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SIM PARAMS</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">failure rate</span><span className="v">{(c.failureRate * 100).toFixed(1)}%</span></div>
            <div className="kv-row"><span className="k">latency ×</span><span className="v">{c.latencyMultiplier}</span></div>
            <div className="kv-row"><span className="k">check count</span><span className="v">7</span></div>
            <div className="kv-row"><span className="k">next run in</span><span className="v">{nextIn === null ? "—" : nextIn + "s"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">failure rate</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">latency ×</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">check count</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">next run in</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">current run</span><span className="v">{agent?.currentRun ? "#" + agent.currentRun.seq : "—"}</span></div>
        <div className="kv-row"><span className="k">checks so far</span><span className="v">{agent?.currentRun ? agent.currentRun.results.filter((r) => r.status !== "pending").length + " / " + agent.currentRun.results.length : "—"}</span></div>
        <div className="kv-row"><span className="k">history</span><span className="v">{agent?.history.length ?? 0} runs</span></div>
      </div>
    </div>
  );
}

function truncate(s: string, n: number): string {
  return s.length <= n ? s : s.slice(0, n - 1) + "…";
}
