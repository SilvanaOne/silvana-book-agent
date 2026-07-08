"use client";

import type { LiquiditySeekingState } from "@/lib/liquidityseeking-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function pct(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return `${n.toFixed(1)}%`;
}

type Props = Readonly<{ liquidityseeking: LiquiditySeekingState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ liquidityseeking, walk }: Props) {
  const c = liquidityseeking?.config;
  const filled = liquidityseeking?.totalFilled ?? 0;
  const total = c?.total ?? 0;
  const remaining = Math.max(0, total - filled);
  const pctFilled = total > 0 ? (filled / total) * 100 : 0;

  const bestBid = liquidityseeking?.book.bids[0]?.price;
  const bestOffer = liquidityseeking?.book.offers[0]?.price;
  const spread = bestBid !== undefined && bestOffer !== undefined ? bestOffer - bestBid : undefined;
  const spreadBps = spread !== undefined && liquidityseeking ? (spread / liquidityseeking.currentPrice) * 10000 : undefined;

  const active = liquidityseeking?.currentChild ?? null;
  const runtimeSecs = liquidityseeking ? Math.max(0, Math.floor(((liquidityseeking.completedAt ?? Date.now()) - liquidityseeking.startedAt) / 1000)) : 0;

  const statusLabel =
    liquidityseeking === null ? "idle"
      : liquidityseeking.status === "completed" ? "completed"
      : liquidityseeking.status === "idle" ? "stopped"
      : active ? "working child"
      : "probing";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">liquidity-seeking</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">elapsed</span><span className="v">{runtimeSecs}s{c?.maxRuntimeSecs ? ` / ${c.maxRuntimeSecs}s` : ""}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LS CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">side</span><span className={`v ${c.side === "buy" ? "positive" : "negative"}`}>{c.side}</span></div>
            <div className="kv-row"><span className="k">total</span><span className="v">{fmt(c.total)}</span></div>
            <div className="kv-row"><span className="k">max slippage</span><span className="v accent">{c.maxSlippageBps} bps</span></div>
            <div className="kv-row"><span className="k">max chunk</span><span className="v">{fmt(c.maxChunk)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">total</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max slippage</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max chunk</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PROGRESS</div></div>
        <div className="kv-row"><span className="k">total filled</span><span className="v accent">{fmt(filled)}</span></div>
        <div className="kv-row"><span className="k">remaining</span><span className="v">{fmt(remaining)}</span></div>
        <div className="kv-row"><span className="k">% complete</span><span className="v">{pct(pctFilled)}</span></div>
        <div className="kv-row"><span className="k">children placed</span><span className="v">{liquidityseeking?.childrenPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">children filled</span><span className="v">{liquidityseeking?.childrenFilled ?? 0}</span></div>
        <div style={{ marginTop: 8, background: "#22222c", height: 6, borderRadius: 3, overflow: "hidden" }}>
          <div style={{ width: `${Math.min(100, pctFilled)}%`, height: "100%", background: "var(--accent)" }} />
        </div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CURRENT CHILD</div></div>
        {active ? (
          <>
            <div className="kv-row"><span className="k">seq</span><span className="v">#{active.seq}</span></div>
            <div className="kv-row"><span className="k">side / qty</span><span className={`v ${active.side === "BID" ? "positive" : "negative"}`}>{active.side} {fmt(active.qty)}</span></div>
            <div className="kv-row"><span className="k">vwap</span><span className="v">{fmt(active.vwap)}</span></div>
            <div className="kv-row"><span className="k">slippage</span><span className="v accent">{active.slippageBps.toFixed(2)} bps</span></div>
            <div className="kv-row"><span className="k">status</span><span className="v">{active.status}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">seq</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side / qty</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">vwap</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">slippage</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">status</span><span className="v faint">idle</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BOOK</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(liquidityseeking?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">best bid</span><span className="v positive">{fmt(bestBid)}</span></div>
        <div className="kv-row"><span className="k">best offer</span><span className="v negative">{fmt(bestOffer)}</span></div>
        <div className="kv-row"><span className="k">spread</span><span className="v">{fmt(spread)}</span></div>
        <div className="kv-row"><span className="k">spread bps</span><span className="v">{spreadBps !== undefined ? spreadBps.toFixed(1) : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{statusLabel}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">book depth</span><span className="v">{c?.depth ?? "—"} lvls</span></div>
      </div>
    </div>
  );
}
