"use client";

import { useCallback, useEffect, useState } from "react";
import { VolatilityScreeningForm, type FormValues } from "./components/VolatilityScreeningForm";
import { VolatilityScreeningChart } from "./components/VolatilityScreeningChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { VolatilityScreeningState } from "@/lib/volatilityscreening-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  volatilityscreening: VolatilityScreeningState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ volatilityscreening: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/volatility-screening-parkinson/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      window: v.window,
      periodSecs: v.periodSecs,
      periodsPerYear: v.periodsPerYear,
      pollSecs: v.pollSecs,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/volatility-screening-parkinson/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/volatility-screening-parkinson/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/volatility-screening-parkinson/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.volatilityscreening?.status === "monitoring";
  const hasMr = snap.volatilityscreening !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid volatilityscreening={snap.volatilityscreening} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Volatility setup</h2>
                  <VolatilityScreeningForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools volatilityscreening={snap.volatilityscreening} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · Parkinson sigma · bar-close markers · <span className="demobadge">DEMO</span></h2>
                  <VolatilityScreeningChart ticks={snap.ticks} volatilityscreening={snap.volatilityscreening} />
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
              <p><strong>agent-volatility-screening-parkinson</strong> — a read-only observability agent that publishes the <em>Parkinson (1980) range-based</em> realized-volatility estimate for a Canton market. Instead of close-to-close log returns, this variant estimates volatility from the high/low range of each closed bar.</p>
              <p>Per tick the simulator samples the mid and updates the currently open bar&rsquo;s H/L. When <span className="mono">periodSecs</span> elapse the bar closes and is pushed into a rolling window of the last <span className="mono">window</span> bars. Once at least two bars have closed the agent publishes:</p>
              <ul style={{ paddingLeft: 18 }}>
                <li><span className="mono">sigma&sup2; = (1 / (4 &middot; ln 2)) &middot; mean( ln(H/L)&sup2; )</span> (per period)</li>
                <li><span className="mono">sigma_annualized = sqrt(sigma&sup2;) &middot; sqrt(periodsPerYear)</span></li>
              </ul>
              <p>For the same window length, the Parkinson estimator is roughly 5&times; more efficient than close-to-close std-dev on a driftless log-normal process&mdash;so the sigma curve responds faster and jitters less. The Rust logic in <span className="mono">crates/agent-volatility-screening-parkinson/src/main.rs</span> uses best-bid/ask as the intra-period range proxy; the demo uses per-tick mids because the GBM simulator has no book.</p>
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
