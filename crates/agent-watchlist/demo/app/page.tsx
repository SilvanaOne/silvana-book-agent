"use client";

import { useCallback, useEffect, useState } from "react";
import { WatchlistForm, type FormValues } from "./components/WatchlistForm";
import { WatchlistChart } from "./components/WatchlistChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { WatchlistState } from "@/lib/watchlist-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  watchlist: WatchlistState | null;
  ticks: Tick[];
  perMarketTicks: Record<string, Tick[]>;
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ watchlist: null, ticks: [], perMarketTicks: {}, events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/watchlist/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      markets: v.markets,
      depthLevels: v.depthLevels,
      includePrices: v.includePrices,
      includeOrderbook: v.includeOrderbook,
      pollSecs: v.pollSecs,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/watchlist/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/watchlist/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/watchlist/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.watchlist?.status === "monitoring";
  const hasMr = snap.watchlist !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid watchlist={snap.watchlist} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Watchlist setup</h2>
                  <WatchlistForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools watchlist={snap.watchlist} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Per-market mid sparklines · <span className="demobadge">DEMO</span></h2>
                  <WatchlistChart perMarketTicks={snap.perMarketTicks} watchlist={snap.watchlist} />
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
              <p><strong>agent-watchlist</strong> — a read-only Canton market monitor. It subscribes to Silvana&rsquo;s external <span className="mono">StreamPrices</span> feed and internal <span className="mono">SubscribeOrderbook</span> depth for every configured market and logs every update. It never places orders, never settles, and never writes to the ledger — pure observability.</p>
              <p>This demo (<span className="mono">lib/watchlist-engine.ts</span>) mirrors the Rust <span className="mono">run_watchlist</span> loop in <span className="mono">crates/agent-watchlist/src/main.rs</span>: on every poll it emits one <span className="mono">PRICE</span> line per market (when the price stream is enabled) and one <span className="mono">DEPTH</span> line per market (when orderbook is enabled). Each market walks an independent GBM path — no real orders sent.</p>
              <p>Use <em>Watchlist setup</em> to configure the markets CSV, poll interval, depth levels, and which streams to subscribe to. The dashboard shows a mini price sparkline per market, top movers, and running counts of every update category.</p>
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
