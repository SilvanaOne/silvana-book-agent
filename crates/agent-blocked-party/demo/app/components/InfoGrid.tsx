"use client";

import type { BlockedPartyState } from "@/lib/blockedparty-engine";

function pctStr(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return `${n.toFixed(1)}%`;
}

function timeShort(t: number | null | undefined): string {
  if (!t) return "—";
  const d = new Date(t);
  const hh = d.getHours().toString().padStart(2, "0");
  const mm = d.getMinutes().toString().padStart(2, "0");
  const ss = d.getSeconds().toString().padStart(2, "0");
  return `${hh}:${mm}:${ss}`;
}

type Props = Readonly<{ blockedparty: BlockedPartyState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ blockedparty, walk }: Props) {
  const c = blockedparty?.config;
  const stateLabel = blockedparty === null ? "idle" : blockedparty.status === "monitoring" ? "monitoring" : "stopped";
  const total = blockedparty?.stats.total ?? 0;
  const blocked = blockedparty?.stats.blocked ?? 0;
  const cleared = blockedparty?.stats.cleared ?? 0;
  const hitsCount = blockedparty?.stats.hitsCount ?? 0;
  const hitRate = total > 0 ? (blocked / total) * 100 : null;
  const runtimeBlocklist = blockedparty?.blocklist ?? [];
  const topEntries = runtimeBlocklist.slice(0, 5);
  const recentHits = blockedparty?.hits.slice(-3).reverse() ?? [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">blocked-party</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BP CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">blocklist size</span><span className="v">{c.blocklist.length}</span></div>
            <div className="kv-row"><span className="k">reload secs</span><span className="v">{c.reloadSecs}</span></div>
            <div className="kv-row"><span className="k">arrival λ</span><span className="v">{c.settlementArrivalPerTick}/tick</span></div>
            <div className="kv-row"><span className="k">hit probability</span><span className="v accent">{(c.blockedPartyProbability * 100).toFixed(1)}%</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">blocklist size</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">reload secs</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">arrival λ</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">hit probability</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">settlements watched</span><span className="v">{total}</span></div>
        <div className="kv-row"><span className="k">hits count</span><span className="v accent">{hitsCount}</span></div>
        <div className="kv-row"><span className="k">hit rate</span><span className={`v ${hitRate !== null && hitRate > 5 ? "accent" : ""}`}>{pctStr(hitRate)}</span></div>
        <div className="kv-row"><span className="k">walk vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BLOCKLIST</div></div>
        {topEntries.length === 0 && (
          <div className="kv-row"><span className="k faint">(empty)</span><span className="v faint">—</span></div>
        )}
        {topEntries.map((e, i) => (
          <div className="kv-row" key={i}>
            <span className="k mono" style={{ fontSize: 12 }}>{e}</span>
            <span className="v faint">#{i + 1}</span>
          </div>
        ))}
        {runtimeBlocklist.length > topEntries.length && (
          <div className="kv-row"><span className="k faint">… and {runtimeBlocklist.length - topEntries.length} more</span><span className="v faint">—</span></div>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">cleared</span><span className="v positive">{cleared}</span></div>
        <div className="kv-row"><span className="k">blocked</span><span className="v negative">{blocked}</span></div>
        <div className="kv-row"><span className="k">reloads</span><span className="v">{blockedparty?.reloadsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">last reload at</span><span className="v">{timeShort(blockedparty?.lastReloadAt ?? null)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RECENT HITS</div></div>
        {recentHits.length === 0 && (
          <div className="kv-row"><span className="k faint">(none)</span><span className="v faint">—</span></div>
        )}
        {recentHits.map((h) => (
          <div className="kv-row" key={`${h.settlementId}-${h.side}`}>
            <span className="k mono" style={{ fontSize: 12 }}>#{h.settlementId} {h.side}</span>
            <span className="v negative mono" style={{ fontSize: 12 }}>{h.blockedParty}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
