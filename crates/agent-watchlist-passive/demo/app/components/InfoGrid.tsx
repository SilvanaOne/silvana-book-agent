"use client";

import type { WatchlistState } from "@/lib/watchlist-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function pct(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(2)}%`;
}

function timeAgo(t: number | null | undefined, now: number): string {
  if (!t) return "—";
  const secs = Math.max(0, Math.floor((now - t) / 1000));
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.floor(secs / 60);
  return `${mins}m ${secs % 60}s ago`;
}

function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

type Props = Readonly<{ watchlist: WatchlistState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ watchlist, walk }: Props) {
  const c = watchlist?.config;
  const now = Date.now();
  const snaps = watchlist?.snapshots ?? [];
  const uptimeSecs = watchlist ? Math.max(0, Math.floor((now - watchlist.startedAt) / 1000)) : 0;

  // Top movers
  let bestGainer: { market: string; pct: number } | null = null;
  let bestLoser: { market: string; pct: number } | null = null;
  for (const s of snaps) {
    if (!bestGainer || s.priceChangeSinceStart > bestGainer.pct) bestGainer = { market: s.market, pct: s.priceChangeSinceStart };
    if (!bestLoser || s.priceChangeSinceStart < bestLoser.pct) bestLoser = { market: s.market, pct: s.priceChangeSinceStart };
  }

  // Updates-per-minute (approximate — uses total updates over uptime)
  const uptimeMin = Math.max(uptimeSecs / 60, 1 / 60);
  const priceRatePm = watchlist ? watchlist.priceUpdatesCount / uptimeMin : 0;
  const depthRatePm = watchlist ? watchlist.depthUpdatesCount / uptimeMin : 0;
  const totalRatePm = watchlist ? watchlist.updatesCount / uptimeMin : 0;

  const streamsChip = c
    ? [c.includePrices ? "prices" : null, c.includeOrderbook ? "depth" : null].filter(Boolean).join(" | ") || "none"
    : "—";
  const stateLabel = watchlist === null ? "idle" : watchlist.status === "monitoring" ? "streaming" : "stopped";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">watchlist</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">{c ? `${c.pollSecs}s` : "—"}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v accent">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">WL CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v">{c.markets.length}</span></div>
            <div className="kv-row"><span className="k">depth levels</span><span className="v">{c.depthLevels}</span></div>
            <div className="kv-row"><span className="k">poll secs</span><span className="v">{c.pollSecs}</span></div>
            <div className="kv-row"><span className="k">streams</span><span className="v accent">{streamsChip}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">depth levels</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">poll secs</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">streams</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRACKED</div></div>
        {snaps.length === 0 ? (
          <div className="kv-row"><span className="k">—</span><span className="v faint">no markets</span></div>
        ) : (
          snaps.map((s) => (
            <div key={s.market} className="kv-row">
              <span className="k">{s.market}</span>
              <span className="v">{fmt(s.price)}</span>
            </div>
          ))
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">UPDATES</div></div>
        <div className="kv-row"><span className="k">price updates</span><span className="v">{watchlist?.priceUpdatesCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">depth updates</span><span className="v">{watchlist?.depthUpdatesCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">total / min</span><span className="v accent">{totalRatePm.toFixed(1)}</span></div>
        <div className="kv-row"><span className="k">rate breakdown</span><span className="v">{priceRatePm.toFixed(1)}p / {depthRatePm.toFixed(1)}d</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP MOVERS</div></div>
        <div className="kv-row"><span className="k">best gainer</span><span className={`v ${bestGainer && bestGainer.pct >= 0 ? "positive" : ""}`}>{bestGainer ? `${bestGainer.market} ${pct(bestGainer.pct)}` : "—"}</span></div>
        <div className="kv-row"><span className="k">best loser</span><span className={`v ${bestLoser && bestLoser.pct < 0 ? "negative" : ""}`}>{bestLoser ? `${bestLoser.market} ${pct(bestLoser.pct)}` : "—"}</span></div>
        <div className="kv-row"><span className="k">walk drift/tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">walk vol/tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">updates count</span><span className="v">{watchlist?.updatesCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">last update</span><span className="v">{timeAgo(watchlist?.lastUpdateAt ?? null, now)}</span></div>
        <div className="kv-row"><span className="k">uptime</span><span className="v accent">{watchlist ? formatUptime(uptimeSecs) : "—"}</span></div>
        <div className="kv-row"><span className="k">ledger writes</span><span className="v">0 (read-only)</span></div>
      </div>
    </div>
  );
}
