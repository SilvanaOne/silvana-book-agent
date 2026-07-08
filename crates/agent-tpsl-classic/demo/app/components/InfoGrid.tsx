"use client";

import type { PositionState } from "@/lib/tpsl-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ position: PositionState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ position, walk }: Props) {
  const cfg = position?.config;
  const pnl = position?.pnl ?? null;
  const pnlPct =
    cfg && position && cfg.entryPrice > 0
      ? ((position.currentPrice - cfg.entryPrice) / cfg.entryPrice) * 100 * (cfg.side === "long" ? 1 : -1)
      : null;
  const pnlCls = pnl === null ? "" : pnl > 0 ? "accent" : "";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">tp/sl monitor</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">POSITION</div></div>
        {cfg ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{cfg.market}</span></div>
            <div className="kv-row"><span className="k">side</span><span className={`v ${cfg.side === "long" ? "accent" : ""}`}>{cfg.side.toUpperCase()}</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v">{cfg.quantity}</span></div>
            <div className="kv-row"><span className="k">entry price</span><span className="v">{fmt(cfg.entryPrice)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">entry price</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current price</span><span className="v">{fmt(position?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">{cfg?.side === "short" ? "trough" : "peak"}</span><span className="v">{fmt(position?.peakPrice)}</span></div>
        <div className="kv-row"><span className="k">unrealized pnl</span><span className={`v ${pnlCls}`}>{pnl === null ? "—" : `${pnl > 0 ? "+" : ""}${fmt(pnl, 6)}`}</span></div>
        <div className="kv-row"><span className="k">pnl %</span><span className={`v ${pnlCls}`}>{pnlPct === null ? "—" : `${pnlPct >= 0 ? "+" : ""}${pnlPct.toFixed(2)}%`}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRADE CONFIG</div></div>
        <div className="kv-row"><span className="k">take profit</span><span className="v accent">{fmt(cfg?.tp)}</span></div>
        <div className="kv-row"><span className="k">stop loss (initial)</span><span className="v">{fmt(cfg?.sl)}</span></div>
        <div className="kv-row"><span className="k">trailing stop</span><span className="v">{cfg?.trailingPct ? `${cfg.trailingPct}%` : "off"}</span></div>
        <div className="kv-row"><span className="k">current SL</span><span className="v accent">{fmt(position?.currentSl)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RISK LIMITS</div></div>
        <div className="kv-row"><span className="k">exit action</span><span className="v">{cfg?.side === "long" ? "SELL on trigger" : cfg?.side === "short" ? "BUY on trigger" : "—"}</span></div>
        <div className="kv-row"><span className="k">exit slippage</span><span className="v">market</span></div>
        <div className="kv-row"><span className="k">max lose</span><span className="v">{cfg?.sl && cfg?.entryPrice ? `${Math.abs((cfg.sl - cfg.entryPrice) / cfg.entryPrice * 100).toFixed(2)}%` : "—"}</span></div>
        <div className="kv-row"><span className="k">daily attempts</span><span className="v">1</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{position?.status === "monitoring" ? "streaming" : "paused"}</span></div>
      </div>
    </div>
  );
}
