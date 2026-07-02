"use client";

import type { YieldRotationState } from "@/lib/yieldrotation-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 6;
  return n.toFixed(digits);
}

function pct(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(3)}%`;
}

function humanVol(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(2)}k`;
  return n.toFixed(0);
}

type Props = Readonly<{ yieldrotation: YieldRotationState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ yieldrotation, walk }: Props) {
  const c = yieldrotation?.config;
  const running = yieldrotation?.status === "monitoring";
  const stateLabel = yieldrotation === null ? "idle" : running ? "running" : "stopped";

  const top = yieldrotation?.ranking[0];
  const nextInMs = yieldrotation ? Math.max(0, yieldrotation.nextPublishAt - Date.now()) : 0;
  const nextIn = Math.ceil(nextInMs / 1000);

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">yield-rotation</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">YR CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v">{c.markets.length}</span></div>
            <div className="kv-row"><span className="k">w_change</span><span className="v">{c.wChange}</span></div>
            <div className="kv-row"><span className="k">w_volume</span><span className="v">{c.wVolume}</span></div>
            <div className="kv-row"><span className="k">w_spread</span><span className="v">{c.wSpread}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">w_change</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">w_volume</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">w_spread</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP RANK</div></div>
        <div className="kv-row"><span className="k">current top</span><span className="v accent">{yieldrotation?.currentTop ?? "—"}</span></div>
        <div className="kv-row"><span className="k">top score</span><span className="v">{fmt(top?.score, 2)}</span></div>
        <div className="kv-row"><span className="k">previous top</span><span className="v">{yieldrotation?.prevTop ?? "—"}</span></div>
        <div className="kv-row"><span className="k">rotations</span><span className="v accent">{yieldrotation?.rotationsCount ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">METRICS</div></div>
        {yieldrotation && yieldrotation.ranking.length > 0 ? (
          yieldrotation.ranking.map((r) => (
            <div key={r.market} className="kv-row">
              <span className="k">{r.market}</span>
              <span className="v mono" style={{ fontSize: 11 }}>
                s={r.score.toFixed(2)} · Δ24h={pct(r.change24hPct)} · v={humanVol(r.volume24h)} · sp={r.spreadPct.toFixed(3)}%
              </span>
            </div>
          ))
        ) : (
          <div className="kv-row"><span className="k">status</span><span className="v faint">no ranking yet</span></div>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CADENCE</div></div>
        <div className="kv-row"><span className="k">poll</span><span className="v">{c ? `${c.pollSecs}s` : "—"}</span></div>
        <div className="kv-row"><span className="k">published</span><span className="v">{yieldrotation?.publishedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">rotations</span><span className="v">{yieldrotation?.rotationsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">next publish</span><span className="v">{yieldrotation && running ? `in ${nextIn}s` : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">per-market GBM</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">starting price</span><span className="v">{c ? fmt(c.startingPrice) : "—"}</span></div>
      </div>
    </div>
  );
}
