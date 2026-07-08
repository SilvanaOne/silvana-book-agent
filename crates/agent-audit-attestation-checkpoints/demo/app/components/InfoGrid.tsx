"use client";

import type { AttestState } from "@/lib/attest-engine";

type Props = Readonly<{ agent: AttestState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const last = agent?.checkpoints[agent.checkpoints.length - 1];
  const head = agent?.history[agent.history.length - 1];
  const total = agent ? agent.totalPublished + agent.totalFailed : 0;
  const successPct = total > 0 ? ((agent!.totalPublished / total) * 100).toFixed(1) + "%" : "—";
  const nextIn = agent?.nextCheckpointAt ? Math.max(0, Math.round((agent.nextCheckpointAt - Date.now()) / 1000)) : null;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">audit-attestation</span></div>
        <div className="kv-row"><span className="k">signing</span><span className="v">ed25519-sha256-v1</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">party</span><span className="v accent">{c?.party ?? "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SCHEDULE</div></div>
        <div className="kv-row"><span className="k">interval</span><span className="v">{c?.intervalSecs === 0 ? "manual only" : c ? c.intervalSecs + "s" : "—"}</span></div>
        <div className="kv-row"><span className="k">next in</span><span className="v">{nextIn === null ? "manual" : nextIn + "s"}</span></div>
        <div className="kv-row"><span className="k">history rate</span><span className="v">{c?.historyGrowthPerSec ?? "—"} rec/s</span></div>
        <div className="kv-row"><span className="k">fail rate</span><span className="v">{c ? (c.webhookFailureRate * 100).toFixed(1) + "%" : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">HISTORY HEAD</div></div>
        {head ? (<>
          <div className="kv-row"><span className="k">seq</span><span className="v">#{head.seq}</span></div>
          <div className="kv-row"><span className="k">ts</span><span className="v mono" style={{ fontSize: 10 }}>{head.ts.slice(11, 19)}</span></div>
          <div className="kv-row"><span className="k">kind</span><span className="v mono" style={{ fontSize: 11 }}>{head.kind}</span></div>
          <div className="kv-row"><span className="k">line hash</span><span className="v mono" style={{ fontSize: 10 }}>{head.line_hash.slice(0, 8)}…{head.line_hash.slice(-4)}</span></div>
        </>) : (<>
          <div className="kv-row"><span className="k">seq</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">ts</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">kind</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">line hash</span><span className="v faint">—</span></div>
        </>)}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LAST CHECKPOINT</div></div>
        {last ? (<>
          <div className="kv-row"><span className="k">seq</span><span className="v accent">#{last.seq}</span></div>
          <div className="kv-row"><span className="k">head_seq</span><span className="v">#{last.headSeq}</span></div>
          <div className="kv-row"><span className="k">head_hash</span><span className="v mono" style={{ fontSize: 10 }}>{last.headHash.slice(0, 10)}…</span></div>
          <div className="kv-row"><span className="k">records</span><span className="v">{last.records}</span></div>
        </>) : (<>
          <div className="kv-row"><span className="k">seq</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">head_seq</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">head_hash</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">records</span><span className="v faint">—</span></div>
        </>)}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">DELIVERY</div></div>
        <div className="kv-row"><span className="k">published</span><span className="v positive">{agent?.totalPublished ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed</span><span className={`v ${(agent?.totalFailed ?? 0) > 0 ? "negative" : ""}`}>{agent?.totalFailed ?? 0}</span></div>
        <div className="kv-row"><span className="k">success rate</span><span className="v">{successPct}</span></div>
        <div className="kv-row"><span className="k">buffer</span><span className="v">{agent?.checkpoints.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "attesting" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">history size</span><span className="v">{agent?.history.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">next record</span><span className="v mono" style={{ fontSize: 11 }}>#{agent?.nextRecordSeq ?? 1}</span></div>
        <div className="kv-row"><span className="k">next checkpoint</span><span className="v mono" style={{ fontSize: 11 }}>#{agent?.nextCheckpointSeq ?? 1}</span></div>
      </div>
    </div>
  );
}
