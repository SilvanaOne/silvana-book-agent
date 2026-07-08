"use client";

import { useCallback, useEffect, useState } from "react";
import { PortfolioHealthForm, type FormValues } from "./components/PortfolioHealthForm";
import { PortfolioHealthChart } from "./components/PortfolioHealthChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { PortfolioHealthState } from "@/lib/portfoliohealth-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  portfoliohealth: PortfolioHealthState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ portfoliohealth: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/portfolio-health-snapshot/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      markets: v.markets,
      instruments: v.instruments,
      snapshotIntervalSecs: v.snapshotIntervalSecs,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/portfolio-health-snapshot/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/portfolio-health-snapshot/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/portfolio-health-snapshot/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.portfoliohealth?.status === "monitoring";
  const hasMr = snap.portfoliohealth !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid portfoliohealth={snap.portfoliohealth} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Portfolio Health setup</h2>
                  <PortfolioHealthForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools portfoliohealth={snap.portfoliohealth} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Balances · Portfolio value · <span className="demobadge">DEMO</span></h2>
                  <PortfolioHealthChart ticks={snap.ticks} portfoliohealth={snap.portfoliohealth} />
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
              <p><strong>agent-portfolio-health-snapshot</strong> — a read-only portfolio reporter. On a schedule it pulls balances from the ledger, active orders and pending settlement proposals from the orderbook, and current market mid-prices, then renders a single human-readable snapshot: balances per instrument, open exposure per market (bid vs offer notional), pending settlements (count + notional + age), and an approximate portfolio NAV in the quote currency of each market.</p>
              <p>The simulation engine (<span className="mono">lib/portfoliohealth-engine.ts</span>) mirrors the Rust reporter in <span className="mono">crates/agent-portfolio-health-snapshot/src/main.rs</span>. Prices follow a GBM random walk, balances drift slowly under simulated fills, and every <span className="mono">snapshot interval</span> a <span className="mono">SNAPSHOT</span> event is emitted. No real orders are placed — this agent is pure observability.</p>
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
