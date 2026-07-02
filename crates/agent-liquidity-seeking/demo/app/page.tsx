"use client";

import { useCallback, useEffect, useState } from "react";
import { LiquiditySeekingForm, type FormValues } from "./components/LiquiditySeekingForm";
import { LiquiditySeekingChart } from "./components/LiquiditySeekingChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { LiquiditySeekingState } from "@/lib/liquidityseeking-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  liquidityseeking: LiquiditySeekingState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ liquidityseeking: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.005 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/liquidity-seeking/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      side: v.side,
      total: v.total,
      maxSlippageBps: v.maxSlippageBps,
      maxChunk: v.maxChunk,
      depth: v.depth,
      maxRuntimeSecs: v.maxRuntimeSecs,
      spreadBps: v.spreadBps,
      depthFalloffPct: v.depthFalloffPct,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/liquidity-seeking/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/liquidity-seeking/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/liquidity-seeking/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.liquidityseeking?.status === "monitoring";
  const hasLs = snap.liquidityseeking !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid liquidityseeking={snap.liquidityseeking} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Liquidity Seeking setup</h2>
                  <LiquiditySeekingForm disabled={running} onStart={start} />
                  {hasLs && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools liquidityseeking={snap.liquidityseeking} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Book depth · active child · fills · <span className="demobadge">DEMO</span></h2>
                  <LiquiditySeekingChart liquidityseeking={snap.liquidityseeking} />
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
              <p><strong>agent-liquidity-seeking</strong> — adaptive execution of a large parent order that keeps VWAP-vs-mid slippage of every child under a fixed budget. Each cycle the agent snapshots the book, walks the relevant side (buy → offers, sell → bids) accumulating quantity until the running VWAP would breach <span className="mono">max-slippage-bps</span>, then places a child sized to <span className="mono">min(safeQty, maxChunk, remaining)</span> at the last accepted price. The agent waits for that child to clear before probing again.</p>
              <p>The execution engine (<span className="mono">lib/liquidityseeking-engine.ts</span>) mirrors the Rust logic in <span className="mono">crates/agent-liquidity-seeking/src/main.rs</span> (see <span className="mono">seek_loop</span> and <span className="mono">max_safe_qty</span>). The book is synthetic: a bid/offer ladder around a GBM-driven mid. No real orders are sent.</p>
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
