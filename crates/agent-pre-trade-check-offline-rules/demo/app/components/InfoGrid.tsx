"use client";

import type { PreTradeCheckState } from "@/lib/pretradecheck-engine";
import { rulesCount } from "@/lib/pretradecheck-engine";

function fmt(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

function pctVal(numer: number, denom: number): string {
  if (denom === 0) return "—";
  return `${((numer / denom) * 100).toFixed(1)}%`;
}

type Props = Readonly<{ pretradecheck: PreTradeCheckState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ pretradecheck, walk }: Props) {
  const c = pretradecheck?.config;
  const stateLabel = pretradecheck === null ? "idle" : pretradecheck.status === "monitoring" ? "checking" : "stopped";
  const stats = pretradecheck?.stats;
  const last = pretradecheck && pretradecheck.checks.length > 0
    ? pretradecheck.checks[pretradecheck.checks.length - 1]
    : null;

  const topFailures = stats
    ? Object.entries(stats.byFailedRule).sort(([, a], [, b]) => b - a).slice(0, 3)
    : [];

  const rulesSummary = c
    ? [
        c.rules.maxNotionalPerOrder !== undefined ? `max_notional=${c.rules.maxNotionalPerOrder}` : null,
        c.rules.maxQuantityPerOrder !== undefined ? `max_qty=${c.rules.maxQuantityPerOrder}` : null,
        c.rules.minPrice !== undefined ? `min_price=${c.rules.minPrice}` : null,
        c.rules.maxPrice !== undefined ? `max_price=${c.rules.maxPrice}` : null,
        c.rules.blockedMarkets && c.rules.blockedMarkets.length > 0
          ? `blocked=[${c.rules.blockedMarkets.join(",")}]`
          : null,
        c.rules.allowedMarkets && c.rules.allowedMarkets.length > 0
          ? `allowed=[${c.rules.allowedMarkets.join(",")}]`
          : null,
        c.rules.allowedSides && c.rules.allowedSides.length > 0
          ? `sides=[${c.rules.allowedSides.join(",")}]`
          : null,
      ].filter((s): s is string => s !== null)
    : [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">pre-trade-check</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PTC CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">active rules</span><span className="v accent">{rulesCount(c.rules)}</span></div>
            <div className="kv-row"><span className="k">order arrival λ</span><span className="v">{c.orderArrivalPerTick}/tick</span></div>
            <div className="kv-row"><span className="k">starting price</span><span className="v">{fmt(c.startingPrice)}</span></div>
            <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(pretradecheck?.currentPrice)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">active rules</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">order arrival λ</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">starting price</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">current mid</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">total checks</span><span className="v">{stats?.total ?? 0}</span></div>
        <div className="kv-row"><span className="k">accepted</span><span className="v positive">{stats?.accepted ?? 0}</span></div>
        <div className="kv-row"><span className="k">rejected</span><span className="v negative">{stats?.rejected ?? 0}</span></div>
        <div className="kv-row"><span className="k">accept rate</span><span className="v accent">{stats ? pctVal(stats.accepted, stats.total) : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP FAILURES</div></div>
        {topFailures.length === 0 ? (
          <>
            <div className="kv-row"><span className="k">—</span><span className="v faint">no failures</span></div>
            <div className="kv-row"><span className="k">—</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">—</span><span className="v faint">—</span></div>
          </>
        ) : (
          topFailures.map(([rule, count]) => (
            <div className="kv-row" key={rule}>
              <span className="k">{rule}</span><span className="v accent">{count}</span>
            </div>
          ))
        )}
        {topFailures.length > 0 && topFailures.length < 3 &&
          Array.from({ length: 3 - topFailures.length }).map((_, i) => (
            <div className="kv-row" key={`pad-${i}`}><span className="k faint">—</span><span className="v faint">—</span></div>
          ))
        }
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LAST CHECK</div></div>
        {last ? (
          <>
            <div className="kv-row"><span className="k">#{last.seq}</span><span className={`v ${last.accepted ? "positive" : "negative"}`}>{last.decision.toUpperCase()}</span></div>
            <div className="kv-row"><span className="k">{last.market}</span><span className="v">{last.side} {fmt(last.quantity)}@{fmt(last.price)}</span></div>
            <div className="kv-row"><span className="k">notional</span><span className="v">{fmt(last.notional)}</span></div>
            <div className="kv-row"><span className="k">failed</span><span className="v mono" style={{ fontSize: 11 }}>{last.failedRules.length ? last.failedRules.join(", ") : "—"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">seq</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">order</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">notional</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">failed</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RULES SUMMARY</div></div>
        {rulesSummary.length === 0 ? (
          <div className="kv-row"><span className="k faint">—</span><span className="v faint">no rules set</span></div>
        ) : (
          rulesSummary.slice(0, 6).map((s) => (
            <div className="kv-row" key={s}>
              <span className="v mono" style={{ fontSize: 11, textAlign: "left", flex: "1 1 auto" }}>{s}</span>
            </div>
          ))
        )}
        <div className="kv-row"><span className="k">walk vol</span><span className="v faint">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
