"use client";

import type { ContractState } from "@/lib/contract-engine";

type Props = Readonly<{ agent: ContractState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const snaps = agent?.snapshots ?? [];
  const underNow = snaps.filter((s) => s.underFloor).length;
  const overNow = snaps.filter((s) => s.overCeiling).length;
  const totalNotional = snaps.reduce((a, s) => a + s.totalInWindow, 0);

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">contractual-compliance</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">status every</span><span className="v">{c ? c.statusIntervalSecs + "s" : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CONTRACTS</div></div>
        <div className="kv-row"><span className="k">count</span><span className="v accent">{c?.contracts.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">under floor</span><span className={`v ${underNow > 0 ? "negative" : "positive"}`}>{underNow}</span></div>
        <div className="kv-row"><span className="k">over ceiling</span><span className={`v ${overNow > 0 ? "negative" : "positive"}`}>{overNow}</span></div>
        <div className="kv-row"><span className="k">expired</span><span className="v">{snaps.filter((s) => s.expired).length}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FLOW</div></div>
        <div className="kv-row"><span className="k">settlements seen</span><span className="v">{agent?.settlementsSeen ?? 0}</span></div>
        <div className="kv-row"><span className="k">rate / sec</span><span className="v">{c?.eventRatePerSec ?? "—"}</span></div>
        <div className="kv-row"><span className="k">parties pool</span><span className="v">{c?.parties.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">markets pool</span><span className="v">{c?.markets.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CUMULATIVE</div></div>
        <div className="kv-row"><span className="k">under events</span><span className="v">{agent?.under ?? 0}</span></div>
        <div className="kv-row"><span className="k">over events</span><span className="v">{agent?.over ?? 0}</span></div>
        <div className="kv-row"><span className="k">cycles</span><span className="v">{agent?.cycle ?? 0}</span></div>
        <div className="kv-row"><span className="k">notional in window</span><span className="v">{totalNotional.toFixed(0)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FIRST 3 SNAPS</div></div>
        {snaps.slice(0, 3).length > 0 ? snaps.slice(0, 3).map((s) => (
          <div key={s.contract.id} className="kv-row">
            <span className="k mono" style={{ fontSize: 11 }}>{truncate(s.contract.id, 18)}</span>
            <span className={`v mono ${s.underFloor || s.overCeiling ? "negative" : "positive"}`} style={{ fontSize: 11 }}>{s.totalInWindow.toFixed(0)}</span>
          </div>
        )) : (
          <>
            <div className="kv-row"><span className="k">contract 1</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">contract 2</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">contract 3</span><span className="v faint">—</span></div>
          </>
        )}
        <div className="kv-row"><span className="k">total tracked</span><span className="v">{snaps.length}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "tracking" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">stream</span><span className="v">SubscribeSettlements</span></div>
        <div className="kv-row"><span className="k">status ticks</span><span className="v">{agent?.snapshots.length ? "flowing" : "warmup"}</span></div>
        <div className="kv-row"><span className="k">reload policy</span><span className="v">on restart</span></div>
      </div>
    </div>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
