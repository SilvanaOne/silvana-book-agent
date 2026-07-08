"use client";

import { useCallback, useEffect, useState } from "react";
import { PortfolioRebalancingForm, type FormValues } from "./components/PortfolioRebalancingForm";
import { PortfolioRebalancingChart } from "./components/PortfolioRebalancingChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { PortfolioRebalancingState } from "@/lib/portfoliorebalancing-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  portfoliorebalancing: PortfolioRebalancingState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ portfoliorebalancing: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/portfolio-rebalancing-target-weights/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    let targets: unknown;
    try {
      targets = JSON.parse(v.targetsJson);
    } catch (ex) {
      throw new Error(`Targets JSON invalid: ${(ex as Error).message}`);
    }
    const body = {
      targets,
      thresholdPct: Number(v.thresholdPct),
      rebalanceFraction: Number(v.rebalanceFraction),
      checkIntervalSecs: Number(v.checkIntervalSecs),
      startingPrice: Number(v.startingPrice),
    };
    const r = await fetch("/api/portfolio-rebalancing-target-weights/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/portfolio-rebalancing-target-weights/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/portfolio-rebalancing-target-weights/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.portfoliorebalancing?.status === "rebalancing";
  const hasState = snap.portfoliorebalancing !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid portfoliorebalancing={snap.portfoliorebalancing} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Rebalancing setup</h2>
                  <PortfolioRebalancingForm disabled={running} onStart={start} />
                  {hasState && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools portfoliorebalancing={snap.portfoliorebalancing} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Current vs Target weights · <span className="demobadge">DEMO</span></h2>
                  <PortfolioRebalancingChart portfoliorebalancing={snap.portfoliorebalancing} />
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
              <p><strong>agent-portfolio-rebalancing-target-weights</strong> — keeps your book on target weights across many instruments. Each cycle it values the portfolio in quote currency via live mids, computes <span className="mono">current_weight = value / total</span> per instrument, and compares against the configured target. If <span className="mono">|current − target| &gt; threshold_pct</span> it places one order: <span className="mono">BID</span> when under-weight, <span className="mono">OFFER</span> when over-weight. Size is <span className="mono">|Δweight| × rebalance_fraction × portfolio_value / mid</span>.</p>
              <p>The placement engine (<span className="mono">lib/portfoliorebalancing-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-portfolio-rebalancing-target-weights/src/main.rs</span>. Per-instrument mids are derived from one base random walk with fixed multipliers (Amulet=1x, CBTC=20000x, CETH=2000x). Fills are simulated when the mid crosses an open order.</p>
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
