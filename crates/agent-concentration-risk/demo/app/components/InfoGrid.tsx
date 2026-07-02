"use client";

import type { ConcentrationRiskState } from "@/lib/concentrationrisk-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ concentrationrisk: ConcentrationRiskState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ concentrationrisk, walk }: Props) {
  const c = concentrationrisk?.config;
  const stateLabel = concentrationrisk === null
    ? "idle"
    : concentrationrisk.breachedInstrument
      ? `breach: ${concentrationrisk.breachedInstrument}`
      : concentrationrisk.status === "monitoring" ? "monitoring" : "stopped";

  const positions = concentrationrisk?.positions ?? [];
  const topShare = positions.reduce((best, p) => (p.sharePct ?? 0) > (best.sharePct ?? 0) ? p : best, positions[0] ?? null);

  const lastCancel = concentrationrisk?.cancellationsHistory.length
    ? concentrationrisk.cancellationsHistory[concentrationrisk.cancellationsHistory.length - 1]
    : null;

  const shareClass = (share: number): string => {
    if (!c) return "";
    if (share > c.maxSharePct) return "negative";
    if (share < c.minSharePct) return "negative";
    if (share > c.maxSharePct - 5 || share < c.minSharePct + 5) return "accent";
    return "positive";
  };

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">concentration-risk</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CR CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">instruments</span><span className="v">{c.instruments.length}</span></div>
            <div className="kv-row"><span className="k">max share</span><span className="v accent">{c.maxSharePct}%</span></div>
            <div className="kv-row"><span className="k">min share</span><span className="v accent">{c.minSharePct}%</span></div>
            <div className="kv-row"><span className="k">dry-run</span><span className="v">{c.dryRun ? "yes" : "no"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">instruments</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max share</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">min share</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">dry-run</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">portfolio value</span><span className="v">{fmt(concentrationrisk?.totalPortfolio)}</span></div>
        <div className="kv-row"><span className="k">top instrument</span><span className="v">{topShare?.name ?? "—"}</span></div>
        <div className="kv-row"><span className="k">top share</span><span className={`v ${topShare ? shareClass(topShare.sharePct ?? 0) : ""}`}>{topShare ? `${(topShare.sharePct ?? 0).toFixed(2)}%` : "—"}</span></div>
        <div className="kv-row"><span className="k">state</span><span className={`v ${concentrationrisk?.breachedInstrument ? "negative" : ""}`}>{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CONCENTRATION</div></div>
        {positions.length ? (
          positions.map((p) => (
            <div key={p.name} className="kv-row">
              <span className="k">{p.name}</span>
              <span className={`v ${shareClass(p.sharePct ?? 0)}`}>{(p.sharePct ?? 0).toFixed(2)}%</span>
            </div>
          ))
        ) : (
          <div className="kv-row"><span className="k">no data</span><span className="v faint">—</span></div>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CANCELS</div></div>
        <div className="kv-row"><span className="k">count</span><span className="v accent">{concentrationrisk?.cancellationsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">last side</span><span className="v">{lastCancel?.side ?? "—"}</span></div>
        <div className="kv-row"><span className="k">last market</span><span className="v">{lastCancel?.market ?? "—"}</span></div>
        <div className="kv-row"><span className="k">orders last</span><span className="v">{lastCancel?.ordersCancelled ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">checks run</span><span className="v">{concentrationrisk?.checksCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">samples</span><span className="v">{concentrationrisk?.samples ?? 0}</span></div>
        <div className="kv-row"><span className="k">walk drift</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">walk vol</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
