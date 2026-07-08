"use client";

import type { OracleState } from "@/lib/oracle-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function fmtTs(t: number | null | undefined): string {
  if (t === null || t === undefined) return "—";
  const d = new Date(t);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

type Props = Readonly<{ oracle: OracleState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ oracle, walk: _walk }: Props) {
  const c = oracle?.config;
  const stateLabel = oracle === null ? "idle" : oracle.status === "publishing" ? "publishing" : "stopped";
  const now = Date.now();
  const nextInSecs = oracle && oracle.status === "publishing" ? Math.max(0, Math.ceil((oracle.nextPublishAt - now) / 1000)) : null;

  // Publish rate (records per minute). Total published is publishedCount.
  const publishRate = oracle && c ? (60 / c.pollSecs) * c.markets.length : 0;

  const recentRecords = oracle?.publishedRecords.slice(-3).reverse() ?? [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">oracle</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ORACLE CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v">{c.markets.length}</span></div>
            <div className="kv-row"><span className="k">source</span><span className="v accent">{c.source}</span></div>
            <div className="kv-row"><span className="k">poll interval</span><span className="v">{c.pollSecs}s</span></div>
            <div className="kv-row"><span className="k">market list</span><span className="v mono" style={{ fontSize: 11 }}>{c.markets.join(", ")}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">source</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">poll interval</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">market list</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE PRICES</div></div>
        {oracle && oracle.currentPrices.length > 0 ? (
          oracle.currentPrices.map((mp) => (
            <div key={mp.market} className="kv-row"><span className="k">{mp.market}</span><span className="v">{fmt(mp.price)}</span></div>
          ))
        ) : (
          <div className="kv-row"><span className="k">—</span><span className="v faint">—</span></div>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">published total</span><span className="v accent">{oracle?.publishedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">publish rate</span><span className="v">{publishRate.toFixed(1)} / min</span></div>
        <div className="kv-row"><span className="k">last publish at</span><span className="v">{fmtTs(oracle?.lastPublishedAt)}</span></div>
        <div className="kv-row"><span className="k">next publish in</span><span className="v">{nextInSecs === null ? "—" : `${nextInSecs}s`}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RECENT</div></div>
        {recentRecords.length > 0 ? (
          recentRecords.map((r, i) => (
            <div key={i} className="kv-row"><span className="k">{r.market}</span><span className="v mono" style={{ fontSize: 11 }}>{fmt(r.price)} @ {fmtTs(r.t)}</span></div>
          ))
        ) : (
          <>
            <div className="kv-row"><span className="k">—</span><span className="v faint">no records</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATUS</div></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">markets tracked</span><span className="v">{oracle?.currentPrices.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">{c?.source ?? "—"}</span></div>
        <div className="kv-row"><span className="k">record buffer</span><span className="v">{oracle?.publishedRecords.length ?? 0} / 60</span></div>
      </div>
    </div>
  );
}
