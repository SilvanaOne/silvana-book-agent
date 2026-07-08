"use client";

import type { FairValueState } from "@/lib/fairvalue-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function relTime(ms: number | undefined, now: number): string {
  if (!ms) return "—";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s ago`;
  return `${Math.floor(s / 3600)}h ago`;
}

type Props = Readonly<{ fairvalue: FairValueState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ fairvalue, walk }: Props) {
  const c = fairvalue?.config;
  const now = Date.now();
  const uptimeSec = fairvalue ? Math.max(0, Math.round((now - fairvalue.startedAt) / 1000)) : 0;
  const stateLabel = fairvalue === null ? "idle" : fairvalue.status === "monitoring" ? "aggregating" : "stopped";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">fair-value</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick cadence</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FV CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">sources</span><span className="v">{c.sources.length}</span></div>
            <div className="kv-row"><span className="k">method</span><span className="v accent">{c.method}</span></div>
            <div className="kv-row"><span className="k">poll secs</span><span className="v">{c.pollSecs}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">sources</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">method</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">poll secs</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE SOURCES</div></div>
        {fairvalue && fairvalue.sourcePrices.length > 0 ? (
          fairvalue.sourcePrices.map((s) => (
            <div key={s.name} className="kv-row">
              <span className="k">{s.name}</span>
              <span className="v">{fmt(s.price)}</span>
            </div>
          ))
        ) : (
          <div className="kv-row"><span className="k">status</span><span className="v faint">idle</span></div>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">AGGREGATION</div></div>
        <div className="kv-row"><span className="k">fair value</span><span className="v accent">{fmt(fairvalue?.fairValue ?? null)}</span></div>
        <div className="kv-row"><span className="k">method</span><span className="v">{c?.method ?? "—"}</span></div>
        <div className="kv-row"><span className="k">samples / cycle</span><span className="v">{fairvalue?.samplesPerCycle ?? 0}</span></div>
        <div className="kv-row"><span className="k">spread across src</span><span className="v">{fairvalue ? `${fairvalue.spreadBpsAcrossSources.toFixed(1)} bps` : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">published</span><span className="v accent">{fairvalue?.publishedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">last published</span><span className="v">{relTime(fairvalue?.lastPublishedAt, now)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">uptime</span><span className="v">{uptimeSec}s</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">true mid</span><span className="v">{fmt(fairvalue ? fairvalue.history.at(-1)?.truth ?? null : null)}</span></div>
        <div className="kv-row"><span className="k">source noise</span><span className="v">±{c?.sourceNoisePct ?? 0}%</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
