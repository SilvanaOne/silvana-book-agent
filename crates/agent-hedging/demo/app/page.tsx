"use client";

import { useCallback, useEffect, useState } from "react";
import { HedgingForm, type FormValues } from "./components/HedgingForm";
import { HedgingChart } from "./components/HedgingChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { HedgingState } from "@/lib/hedging-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  hedging: HedgingState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ hedging: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/hedging/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      exposureInstrument: v.exposureInstrument,
      hedgeMarket: v.hedgeMarket,
      targetBalance: v.targetBalance,
      tolerance: v.tolerance,
      hedgeFraction: v.hedgeFraction,
      checkIntervalSecs: v.checkIntervalSecs,
      exposureDriftPerTick: v.exposureDriftPerTick,
      startingBalance: v.startingBalance,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/hedging/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/hedging/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/hedging/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.hedging?.status === "monitoring";
  const hasHedging = snap.hedging !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid hedging={snap.hedging} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Hedging setup</h2>
                  <HedgingForm disabled={running} onStart={start} />
                  {hasHedging && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools hedging={snap.hedging} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Price · balance · target band · hedges · <span className="demobadge">DEMO</span></h2>
                  <HedgingChart ticks={snap.ticks} hedging={snap.hedging} />
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
              <p><strong>agent-hedging</strong> — neutralizes unwanted directional exposure by pushing the unlocked balance of an instrument back toward a target on Canton. Every <span className="mono">check_interval</span> the agent reads its unlocked balance and computes <span className="mono">delta = balance − target</span>. If <span className="mono">|delta| ≤ tolerance</span>, no hedge is placed. Otherwise it submits one limit order on the hedge market: <span className="mono">OFFER</span> when the balance is above target (sell surplus), <span className="mono">BID</span> when below (buy back). The order size is <span className="mono">|delta| × hedge_fraction</span>, so the agent walks exposure back toward target rather than slamming the book. Only one open hedge per side at a time.</p>
              <p>The placement engine (<span className="mono">lib/hedging-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-hedging/src/main.rs</span>. Prices follow a GBM random walk and the balance drifts by a configurable Gaussian σ each tick to simulate external strategies moving the exposure. Fills are simulated whenever the mid crosses a resting hedge order.</p>
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
