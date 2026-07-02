"use client";

import type { PnlScreeningState } from "@/lib/pnlscreening-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function bp(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(1)} bp`;
}

function pnlClass(n: number | null | undefined): string {
  if (!n) return "";
  return n > 0 ? "positive" : n < 0 ? "negative" : "";
}

function uptime(startedAt: number | null | undefined): string {
  if (!startedAt) return "—";
  const s = Math.max(0, Math.floor((Date.now() - startedAt) / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = s % 60;
  return h > 0 ? `${h}h ${m}m ${ss}s` : m > 0 ? `${m}m ${ss}s` : `${ss}s`;
}

type Props = Readonly<{ pnlscreening: PnlScreeningState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ pnlscreening, walk }: Props) {
  const c = pnlscreening?.config;
  const stateLabel = pnlscreening === null ? "idle" : pnlscreening.status === "monitoring" ? "monitoring" : "stopped";

  const pos = pnlscreening?.position ?? 0;
  const cost = pnlscreening?.avgCostBasis ?? 0;
  const mid = pnlscreening?.currentPrice ?? 0;
  const distBp = pos > 0 && cost > 0 && mid > 0 ? ((mid - cost) / cost) * 10000 : null;
  const posValue = pos > 0 && mid > 0 ? pos * mid : 0;
  const realized = pnlscreening?.realizedPnl ?? 0;
  const unrealized = pnlscreening?.unrealizedPnl ?? 0;
  const total = realized + unrealized;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">pnl-screening</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PL CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">snapshot interval</span><span className="v accent">{c.snapshotIntervalSecs}s</span></div>
            <div className="kv-row"><span className="k">arrival / tick</span><span className="v">{c.tradeArrivalPerTick}</span></div>
            <div className="kv-row"><span className="k">avg trade qty</span><span className="v">{c.avgTradeQty}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">snapshot interval</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">arrival / tick</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">avg trade qty</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">POSITION</div></div>
        <div className="kv-row"><span className="k">quantity</span><span className="v">{fmt(pos)}</span></div>
        <div className="kv-row"><span className="k">avg cost basis</span><span className="v accent">{fmt(cost)}</span></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(mid)}</span></div>
        <div className="kv-row"><span className="k">dist from cost</span><span className="v">{bp(distBp)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">REALIZED</div></div>
        <div className="kv-row"><span className="k">realized pnl</span><span className={`v ${pnlClass(realized)}`}>{fmt(realized)}</span></div>
        <div className="kv-row"><span className="k">buys count</span><span className="v">{pnlscreening?.buysCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">sells count</span><span className="v">{pnlscreening?.sellsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">total trades</span><span className="v">{pnlscreening?.tradesCount ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">UNREALIZED</div></div>
        <div className="kv-row"><span className="k">unrealized pnl</span><span className={`v ${pnlClass(unrealized)}`}>{fmt(unrealized)}</span></div>
        <div className="kv-row"><span className="k">position value</span><span className="v">{fmt(posValue)}</span></div>
        <div className="kv-row"><span className="k">pnl total</span><span className={`v ${pnlClass(total)}`}>{fmt(total)}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">mid × (mid − cost)</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">snapshots</span><span className="v">{pnlscreening?.snapshotsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">uptime</span><span className="v">{uptime(pnlscreening?.startedAt)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
