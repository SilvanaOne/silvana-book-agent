"use client";

import { useCallback, useEffect, useState } from "react";
import { LiquidityScreeningForm, type FormValues } from "./components/LiquidityScreeningForm";
import { LiquidityScreeningChart } from "./components/LiquidityScreeningChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { LiquidityScreeningState } from "@/lib/liquidityscreening-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  liquidityscreening: LiquidityScreeningState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ liquidityscreening: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/liquidity-screening-depth-probe/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      probeQty: v.probeQty,
      depth: v.depth,
      pollSecs: v.pollSecs,
      spreadBps: v.spreadBps,
      depthFalloffPct: v.depthFalloffPct,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/liquidity-screening-depth-probe/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/liquidity-screening-depth-probe/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/liquidity-screening-depth-probe/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.liquidityscreening?.status === "monitoring";
  const hasMr = snap.liquidityscreening !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid liquidityscreening={snap.liquidityscreening} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Liquidity setup</h2>
                  <LiquidityScreeningForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools liquidityscreening={snap.liquidityscreening} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Book depth · spread · slippage probes · <span className="demobadge">DEMO</span></h2>
                  <LiquidityScreeningChart ticks={snap.ticks} liquidityscreening={snap.liquidityscreening} />
                </div>
                <div className="card">
                  <h2>Recent events</h2>
                  <EventsLog events={snap.events.slice(-8)} />
                </div>
              </div>
            </div>
          </>
        )}

        {tab === "events" && (
          <div className="card">
            <h2>Events log</h2>
            <EventsLog events={snap.events} />
          </div>
        )}

        {tab === "docs" && (
          <div className="card">
            <h2>About this demo</h2>
            <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
              <p><strong>agent-liquidity-screening-depth-probe</strong> — polls <span className="mono">GetOrderbookDepth</span> per market and publishes a liquidity snapshot every poll: spread + spread_bps, aggregated bid/offer depth, and a two-sided slippage probe. For a target <span className="mono">probeQty</span> the agent walks the offer side book-level-by-level to compute VWAP for a market buy and the bid side for a market sell, then reports <span className="mono">|vwap − mid| / mid × 10000</span> as slippage in bps. Read-only — no orders, no ledger writes.</p>
              <p>The metric engine (<span className="mono">lib/liquidityscreening-engine.ts</span>) mirrors the Rust logic in <span className="mono">crates/agent-liquidity-screening-depth-probe/src/main.rs</span> (<span className="mono">sample_market</span> + <span className="mono">walk</span>). The synthetic book here breathes with a GBM random-walk mid and level qty jitter, so the depth chart and slippage probes update every tick.</p>
              <ul style={{ paddingLeft: 18 }}>
                <li><a className="mono" style={{ color: "var(--accent)" }} href="https://github.com/SilvanaOne/silvana-book-agent" target="_blank" rel="noopener">github: silvana-book-agent</a></li>
                <li><a className="mono" style={{ color: "var(--accent)" }} href="https://docs.silvana.one" target="_blank" rel="noopener">docs.silvana.one</a></li>
                <li><a className="mono" style={{ color: "var(--accent)" }} href="https://canton.network" target="_blank" rel="noopener">canton.network</a></li>
              </ul>
            </div>
          </div>
        )}
      </div>
      <Footer />
    </>
  );
}
