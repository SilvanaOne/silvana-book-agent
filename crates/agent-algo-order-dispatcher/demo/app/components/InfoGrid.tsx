"use client";

import type { AlgoState } from "@/lib/algo-engine";

type Props = Readonly<{ agent: AlgoState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ agent, walk }: Props) {
  const c = agent?.config;
  const cur = agent?.steps[agent.currentStepIndex];
  const stateLabel = agent === null ? "idle" : agent.status === "running" ? "executing" : agent.status === "done" ? "done" : "stopped";
  const totalTarget = c?.steps.reduce((a, s) => a + s.total, 0) ?? 0;
  const totalDone = agent?.steps.reduce((a, s) => a + s.filled, 0) ?? 0;
  const progressPct = totalTarget > 0 ? (totalDone / totalTarget) * 100 : 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">algo-order</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">plan steps</span><span className="v">{c?.steps.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CURRENT STEP</div></div>
        {cur && agent && agent.currentStepIndex < agent.steps.length ? (
          <>
            <div className="kv-row"><span className="k">index</span><span className="v">#{cur.index + 1} / {agent.steps.length}</span></div>
            <div className="kv-row"><span className="k">algo</span><span className="v accent">{cur.algo}</span></div>
            <div className="kv-row"><span className="k">market</span><span className="v">{cur.market}</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v">{cur.side.toUpperCase()}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">index</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">algo</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PROGRESS</div></div>
        <div className="kv-row"><span className="k">filled total</span><span className="v positive">{totalDone.toFixed(3)}</span></div>
        <div className="kv-row"><span className="k">plan total</span><span className="v">{totalTarget.toFixed(3)}</span></div>
        <div className="kv-row"><span className="k">progress</span><span className="v accent">{progressPct.toFixed(1)}%</span></div>
        <div className="kv-row"><span className="k">notional</span><span className="v">{(agent?.totalNotional ?? 0).toFixed(2)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CHILDREN</div></div>
        <div className="kv-row"><span className="k">placed</span><span className="v">{agent?.totalPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">active</span><span className="v">{agent?.children.filter((c) => c.status === "active").length ?? 0}</span></div>
        <div className="kv-row"><span className="k">filled</span><span className="v positive">{agent?.children.filter((c) => c.status === "filled").length ?? 0}</span></div>
        <div className="kv-row"><span className="k">avg qty</span><span className="v">{avg(agent?.children.map((c) => c.qty) ?? [])}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MID / MARKET</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{agent?.currentPrice.toFixed(6) ?? "—"}</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">book vol</span><span className="v">{((c?.bookVolatility ?? 0) * 100).toFixed(2)}%</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">steps done</span><span className="v">{agent?.steps.filter((s) => s.status === "done").length ?? 0}</span></div>
        <div className="kv-row"><span className="k">steps pending</span><span className="v">{agent?.steps.filter((s) => s.status === "pending").length ?? 0}</span></div>
        <div className="kv-row"><span className="k">runtime</span><span className="v">{runtimeStr(agent)}</span></div>
      </div>
    </div>
  );
}

function avg(nums: number[]): string {
  if (nums.length === 0) return "—";
  const m = nums.reduce((a, b) => a + b, 0) / nums.length;
  return m.toFixed(3);
}

function runtimeStr(agent: AlgoState | null): string {
  if (!agent) return "—";
  const first = agent.steps.find((s) => s.startedAt !== null);
  if (!first || !first.startedAt) return "—";
  return Math.round((Date.now() - first.startedAt) / 1000) + "s";
}
