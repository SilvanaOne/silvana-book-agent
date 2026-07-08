"use client";

import type { RiskState } from "@/lib/riskmgmt-engine";

type Props = Readonly<{ agent: RiskState | null }>;

const W = 720, H = 340;

export function RiskMgmtChart({ agent }: Props) {
  if (!agent) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start monitoring to see notional × policy.</div>;
  }

  const hist = agent.history;
  const last = agent.lastEvent;
  const cap = agent.config.policy.maxOpenNotional ? Number(agent.config.policy.maxOpenNotional) : null;

  // Chart panel dimensions: left half for notional-vs-limit line, right for policy hit tiles.
  const chartW = 460;
  const chartH = 240;
  const pad = { left: 40, right: 20, top: 30, bottom: 30 };

  const values = hist.map((h) => h.openNotional);
  const yMax = Math.max(cap ?? 0, ...values, 1) * 1.15;
  const yMin = 0;
  const points = hist.length > 0 ? hist.map((h, i) => {
    const x = pad.left + (i / Math.max(1, hist.length - 1)) * (chartW - pad.left - pad.right);
    const y = pad.top + (1 - (h.openNotional - yMin) / (yMax - yMin)) * (chartH - pad.top - pad.bottom);
    return `${x},${y}`;
  }).join(" ") : "";

  const capY = cap !== null
    ? pad.top + (1 - (cap - yMin) / (yMax - yMin)) * (chartH - pad.top - pad.bottom)
    : null;

  // Right side: policy check tiles.
  const tilesX = chartW + 20;
  const tilesW = W - tilesX - 10;
  const policy = agent.config.policy;
  const tiles: Array<{ key: string; obs: string; limit: string; hit: boolean }> = [];
  const hitKeys = new Set(last?.hits.map((h) => h.key) ?? []);
  if (typeof policy.maxOpenOrders === "number") tiles.push({
    key: "max_open_orders",
    obs: String(last?.openOrders ?? agent.orders.length),
    limit: String(policy.maxOpenOrders),
    hit: hitKeys.has("max_open_orders"),
  });
  if (policy.maxOpenNotional) tiles.push({
    key: "max_open_notional",
    obs: last?.openNotional ?? "0.00",
    limit: policy.maxOpenNotional,
    hit: hitKeys.has("max_open_notional"),
  });
  if (typeof policy.maxPendingSettlements === "number") tiles.push({
    key: "max_pending",
    obs: String(last?.pending ?? 0),
    limit: String(policy.maxPendingSettlements),
    hit: hitKeys.has("max_pending_settlements"),
  });
  if (typeof policy.maxFailedSettlements === "number") tiles.push({
    key: "max_failed",
    obs: String(last?.failed ?? 0),
    limit: String(policy.maxFailedSettlements),
    hit: hitKeys.has("max_failed_settlements"),
  });
  for (const [market, capStr] of Object.entries(policy.perMarketMaxNotional)) {
    tiles.push({
      key: `perMarket[${market}]`,
      obs: last?.perMarketNotional?.[market] ?? "0.00",
      limit: capStr,
      hit: hitKeys.has(`per_market_max_notional[${market}]`),
    });
  }
  const tileH = Math.min(40, (chartH - 10) / Math.max(tiles.length, 1));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* Left: notional-vs-limit line */}
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        open notional (last {hist.length} cycles)
      </text>
      {/* Grid */}
      <line x1={pad.left} x2={chartW - pad.right} y1={chartH - pad.bottom} y2={chartH - pad.bottom} stroke="var(--border)" strokeWidth={0.6} />
      <line x1={pad.left} x2={pad.left} y1={pad.top} y2={chartH - pad.bottom} stroke="var(--border)" strokeWidth={0.6} />
      <text x={pad.left - 6} y={chartH - pad.bottom + 4} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">0</text>
      <text x={pad.left - 6} y={pad.top + 4} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">{fmtShort(yMax)}</text>

      {points && <polyline points={points} fill="none" stroke="var(--accent)" strokeWidth={1.4} />}
      {capY !== null && (
        <>
          <line x1={pad.left} x2={chartW - pad.right} y1={capY} y2={capY} stroke="var(--neg)" strokeWidth={1} strokeDasharray="4 3" />
          <text x={chartW - pad.right - 4} y={capY - 4} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">limit {cap}</text>
        </>
      )}

      {/* Time-series footer */}
      <text x={chartW / 2} y={H - 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="middle">
        cycles → · enforce = {agent.config.enforce ? "ON" : "OFF"} · total breaches = {agent.totalBreaches}
      </text>

      {/* Right: policy tiles */}
      <text x={tilesX} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">policy checks (this cycle)</text>
      {tiles.length === 0 && (
        <text x={tilesX} y={60} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">No checks configured.</text>
      )}
      {tiles.map((t, i) => {
        const y = 30 + i * (tileH + 6);
        const color = t.hit ? "var(--neg)" : "var(--pos)";
        return (
          <g key={t.key}>
            <rect x={tilesX} y={y} width={tilesW} height={tileH} rx={6} fill="var(--bg-card)" stroke={color} strokeWidth={1.2} />
            <text x={tilesX + 8} y={y + 14} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{t.key}</text>
            <text x={tilesX + tilesW - 8} y={y + 14} fill={color} fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">{t.hit ? "BREACH" : "OK"}</text>
            <text x={tilesX + 8} y={y + tileH - 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{t.obs} <tspan fill="var(--text-faint)">/ limit</tspan> {t.limit}</text>
          </g>
        );
      })}
    </svg>
  );
}

function fmtShort(n: number): string {
  if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "k";
  return n.toFixed(0);
}
