"use client";

import type { RetentionState } from "@/lib/retention-engine";

type Props = Readonly<{ agent: RetentionState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const verify = agent?.lastVerify;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">audit-retention</span></div>
        <div className="kv-row"><span className="k">bucketing</span><span className="v accent">{c?.weekly ? "weekly" : "daily"}</span></div>
        <div className="kv-row"><span className="k">signing</span><span className="v">ed25519-sha256-v1</span></div>
        <div className="kv-row"><span className="k">party</span><span className="v mono" style={{ fontSize: 11 }}>{c?.party ?? "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">INPUT</div></div>
        <div className="kv-row"><span className="k">total days</span><span className="v">{c?.totalDays ?? 0}</span></div>
        <div className="kv-row"><span className="k">records / day</span><span className="v">{c?.recordsPerDay ?? 0}</span></div>
        <div className="kv-row"><span className="k">retention</span><span className="v">{c?.retentionDays === undefined ? "∞" : c?.retentionDays + " d"}</span></div>
        <div className="kv-row"><span className="k">rotations</span><span className="v">{agent?.rotations ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">OUTPUT</div></div>
        <div className="kv-row"><span className="k">slices</span><span className="v accent">{agent?.slices.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">records kept</span><span className="v positive">{agent?.totalRecords ?? 0}</span></div>
        <div className="kv-row"><span className="k">dropped</span><span className="v">{agent?.droppedByRetention ?? 0}</span></div>
        <div className="kv-row"><span className="k">avg / slice</span><span className="v">{agent && agent.slices.length > 0 ? Math.round(agent.totalRecords / agent.slices.length) : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VERIFY</div></div>
        {verify ? (<>
          <div className="kv-row"><span className="k">verdict</span><span className={`v ${verify.ok ? "positive" : "negative"}`}>{verify.ok ? "OK" : "FAIL"}</span></div>
          <div className="kv-row"><span className="k">checked</span><span className="v">{verify.checked}</span></div>
          <div className="kv-row"><span className="k">broken at</span><span className="v">{verify.brokenAt ?? "—"}</span></div>
          <div className="kv-row"><span className="k">reason</span><span className="v mono" style={{ fontSize: 11 }}>{verify.reason ?? "—"}</span></div>
        </>) : (<>
          <div className="kv-row"><span className="k">verdict</span><span className="v faint">not run</span></div>
          <div className="kv-row"><span className="k">checked</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">broken at</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">reason</span><span className="v faint">—</span></div>
        </>)}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FIRST SLICE</div></div>
        {agent?.slices[0] ? (<>
          <div className="kv-row"><span className="k">num</span><span className="v">#{agent.slices[0].num}</span></div>
          <div className="kv-row"><span className="k">bucket</span><span className="v mono" style={{ fontSize: 11 }}>{agent.slices[0].bucket}</span></div>
          <div className="kv-row"><span className="k">records</span><span className="v">{agent.slices[0].recordCount}</span></div>
          <div className="kv-row"><span className="k">hash</span><span className="v mono" style={{ fontSize: 10 }}>{agent.slices[0].sliceHash.slice(0, 12)}…</span></div>
        </>) : (<>
          <div className="kv-row"><span className="k">num</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">bucket</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">records</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">hash</span><span className="v faint">—</span></div>
        </>)}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LAST SLICE</div></div>
        {agent && agent.slices.length > 0 ? (() => {
          const s = agent.slices[agent.slices.length - 1];
          return (<>
            <div className="kv-row"><span className="k">num</span><span className="v accent">#{s.num}</span></div>
            <div className="kv-row"><span className="k">bucket</span><span className="v mono" style={{ fontSize: 11 }}>{s.bucket}</span></div>
            <div className="kv-row"><span className="k">records</span><span className="v">{s.recordCount}</span></div>
            <div className="kv-row"><span className="k">hash</span><span className="v mono" style={{ fontSize: 10 }}>{s.sliceHash.slice(0, 12)}…</span></div>
          </>);
        })() : (<>
          <div className="kv-row"><span className="k">num</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">bucket</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">records</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">hash</span><span className="v faint">—</span></div>
        </>)}
      </div>
    </div>
  );
}
