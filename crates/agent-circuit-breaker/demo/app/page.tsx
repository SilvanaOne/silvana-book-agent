"use client";

import { useCallback, useEffect, useState } from "react";
import { CircuitBreakerForm, type FormValues } from "./components/CircuitBreakerForm";
import { CircuitBreakerChart } from "./components/CircuitBreakerChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { CircuitBreakerState } from "@/lib/circuitbreaker-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  circuitbreaker: CircuitBreakerState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
  stopped?: boolean;
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ circuitbreaker: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/circuit-breaker/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      maxDeviationPct: v.maxDeviationPct,
      windowSecs: v.windowSecs,
      pauseSecs: v.pauseSecs,
      startingPrice: v.startingPrice,
      startingOpenOrders: v.startingOpenOrders,
    };
    const r = await fetch("/api/circuit-breaker/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/circuit-breaker/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/circuit-breaker/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.circuitbreaker !== null && !snap.stopped;
  const hasCb = snap.circuitbreaker !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid circuitbreaker={snap.circuitbreaker} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Circuit Breaker setup</h2>
                  <CircuitBreakerForm disabled={running} onStart={start} />
                  {hasCb && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools circuitbreaker={snap.circuitbreaker} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · baseline · bands · breaches · <span className="demobadge">DEMO</span></h2>
                  <CircuitBreakerChart ticks={snap.ticks} circuitbreaker={snap.circuitbreaker} />
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
              <p><strong>agent-circuit-breaker</strong> — a safety net that halts trading on a market when its mid price diverges from a rolling baseline. Every poll it appends the current mid into a rolling window bounded by <span className="mono">windowSecs</span>, recomputes the baseline as the arithmetic mean of the samples, and evaluates <span className="mono">deviation = (mid − baseline) / baseline × 100</span>. If <span className="mono">|deviation| &gt; maxDeviationPct</span>, the breaker <strong>TRIPS</strong>: it cancels every open order this party holds on that market and pauses for <span className="mono">pauseSecs</span>. After the pause it re-arms with the baseline reset to the current mid.</p>
              <p>The chart shows mid (white), rolling baseline (orange), the ±deviation bands (red dashed), and red X markers on breach ticks. Paused intervals are shaded red. The engine (<span className="mono">lib/circuitbreaker-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-circuit-breaker/src/main.rs</span>. Prices are a GBM random walk — no real orders sent, and cancellations are simulated against a mock open-orders counter you seed in the form.</p>
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
