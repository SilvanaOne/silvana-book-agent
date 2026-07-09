"use client";

import type { PairsTradingState, Signal } from "@/lib/pairstrading-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function fmtZ(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(3)}`;
}

function signalLabel(s: Signal | undefined): string {
  switch (s) {
    case "long-A": return "long-A / short-B";
    case "short-A": return "short-A / long-B";
    default: return "neutral";
  }
}

type Props = Readonly<{ pairstrading: PairsTradingState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ pairstrading, walk }: Props) {
  const c = pairstrading?.config;
  const armed = pairstrading?.status === "monitoring";
  const openA = pairstrading?.orders.filter((o) => o.status === "open" && o.leg === "A").length ?? 0;
  const openB = pairstrading?.orders.filter((o) => o.status === "open" && o.leg === "B").length ?? 0;
  const stateLabel = pairstrading === null ? "idle" : armed ? "armed" : "stopped";
  const z = pairstrading?.z ?? null;
  const beyondEntry = pairstrading && c && z !== null && Math.abs(z) > c.entryZ;
  const bufFilled = pairstrading?.window.length ?? 0;
  const warmup = c?.warmupSamples ?? 0;
  const warming = pairstrading !== null && bufFilled < warmup;
  const pnlClass = (pairstrading?.realizedPnl ?? 0) > 0 ? "positive" : (pairstrading?.realizedPnl ?? 0) < 0 ? "negative" : "";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">pairs-trading · z-score</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PT CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market A</span><span className="v">{c.marketA}</span></div>
            <div className="kv-row"><span className="k">market B</span><span className="v">{c.marketB}</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v accent">{c.window}</span></div>
            <div className="kv-row"><span className="k">entry |z|</span><span className="v accent">{c.entryZ}</span></div>
            <div className="kv-row"><span className="k">exit |z|</span><span className="v">{c.exitZ}</span></div>
            <div className="kv-row"><span className="k">warmup</span><span className="v">{c.warmupSamples}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market A</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">market B</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">entry |z|</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">exit |z|</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">warmup</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LEG A</div></div>
        <div className="kv-row"><span className="k">market</span><span className="v">{c?.marketA ?? "—"}</span></div>
        <div className="kv-row"><span className="k">price</span><span className="v">{fmt(pairstrading?.priceA)}</span></div>
        <div className="kv-row"><span className="k">quantity</span><span className="v">{c?.quantityA ?? "—"}</span></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{openA}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LEG B</div></div>
        <div className="kv-row"><span className="k">market</span><span className="v">{c?.marketB ?? "—"}</span></div>
        <div className="kv-row"><span className="k">price</span><span className="v">{fmt(pairstrading?.priceB)}</span></div>
        <div className="kv-row"><span className="k">quantity</span><span className="v">{c?.quantityB ?? "—"}</span></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{openB}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">Z-SCORE</div></div>
        <div className="kv-row"><span className="k">ratio</span><span className="v">{fmt(pairstrading?.ratio, 6)}</span></div>
        <div className="kv-row"><span className="k">μ (window)</span><span className="v">{fmt(pairstrading?.mean ?? undefined, 6)}</span></div>
        <div className="kv-row"><span className="k">σ (window)</span><span className="v">{fmt(pairstrading?.sigma ?? undefined, 6)}</span></div>
        <div className="kv-row"><span className="k">z</span><span className={`v ${beyondEntry ? "accent" : ""}`}>{fmtZ(z)}</span></div>
        <div className="kv-row"><span className="k">signal</span><span className="v accent">{signalLabel(pairstrading?.signal)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}{warming ? " · warming up" : beyondEntry ? " · beyond entry" : ""}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">samples buffered</span><span className="v">{bufFilled}/{c?.window ?? "—"}</span></div>
        <div className="kv-row"><span className="k">position open</span><span className="v">{pairstrading?.positionOpen ? "yes" : "no"}</span></div>
        <div className="kv-row"><span className="k">signals</span><span className="v">{pairstrading?.signalsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders placed</span><span className="v">{pairstrading?.ordersPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders filled</span><span className="v accent">{pairstrading?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">skipped (stacked)</span><span className="v">{pairstrading?.signalsSkipped ?? 0}</span></div>
        <div className="kv-row"><span className="k">realized pnl</span><span className={`v ${pnlClass}`}>{fmt(pairstrading?.realizedPnl)}</span></div>
        <div className="kv-row"><span className="k">walk vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
