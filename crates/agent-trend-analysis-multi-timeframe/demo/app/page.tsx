"use client";

import { useCallback, useEffect, useState } from "react";
import { TrendAnalysisForm, type FormValues } from "./components/TrendAnalysisForm";
import { TrendAnalysisChart } from "./components/TrendAnalysisChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { TrendAnalysisState } from "@/lib/trendanalysis-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  trendanalysis: TrendAnalysisState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ trendanalysis: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/trend-analysis-multi-timeframe/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      short: v.short,
      mid: v.mid,
      long: v.long,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/trend-analysis-multi-timeframe/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/trend-analysis-multi-timeframe/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/trend-analysis-multi-timeframe/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.trendanalysis?.status === "monitoring";
  const hasTa = snap.trendanalysis !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid trendanalysis={snap.trendanalysis} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Multi-timeframe setup</h2>
                  <TrendAnalysisForm disabled={running} onStart={start} />
                  {hasTa && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools trendanalysis={snap.trendanalysis} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · SMA short · SMA mid · SMA long · <span className="demobadge">DEMO</span></h2>
                  <TrendAnalysisChart ticks={snap.ticks} trendanalysis={snap.trendanalysis} />
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
              <p><strong>agent-trend-analysis-multi-timeframe</strong> — read-only multi-timeframe confluence publisher for Silvana Orderbook on Canton. On every poll it pushes the current mid into a rolling buffer capped at <span className="mono">long</span> samples and recomputes three simple moving averages:</p>
              <ul style={{ paddingLeft: 18 }}>
                <li><span className="mono">SMA(short)</span> — fast timeframe, reacts to recent bars</li>
                <li><span className="mono">SMA(mid)</span> — swing timeframe</li>
                <li><span className="mono">SMA(long)</span> — slow structural timeframe (also the buffer capacity)</li>
              </ul>
              <p>Each tick the agent also computes the slope of every SMA versus the previous tick and derives an <span className="mono">alignment</span> verdict:</p>
              <ul style={{ paddingLeft: 18 }}>
                <li><span className="mono">aligned_up</span> — all three slopes strictly positive (trend confluence long)</li>
                <li><span className="mono">aligned_down</span> — all three slopes strictly negative (trend confluence short)</li>
                <li><span className="mono">mixed</span> — timeframes disagree; downstream strategies should stand aside</li>
              </ul>
              <p>Warmup: nothing is emitted until the buffer holds at least <span className="mono">long</span> samples so every SMA is well-defined. The engine (<span className="mono">lib/trendanalysis-engine.ts</span>) mirrors <span className="mono">crates/agent-trend-analysis-multi-timeframe/src/main.rs</span>. Every tick emits one <span className="mono">MTF</span> record; alignment changes are highlighted as <span className="mono">SIGNAL</span> events. Prices here are a GBM random walk — no ledger writes.</p>
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
