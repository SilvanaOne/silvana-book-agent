"use client";

import { useCallback, useEffect, useState } from "react";
import { DcaPortfolioForm, type FormValues } from "./components/DcaPortfolioForm";
import { DcaPortfolioChart } from "./components/DcaPortfolioChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { DcaPortfolioState } from "@/lib/dcaportfolio-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  dcaportfolio: DcaPortfolioState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({
    dcaportfolio: null,
    ticks: [],
    events: [],
    walk: { driftPerTick: 0, volPerTick: 0.008 },
  });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/dca-portfolio/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 1000);
    return () => clearInterval(id);
  }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      markets: v.markets,
      side: v.side,
      amountPerOrder: v.amountPerOrder,
      intervalSecs: v.intervalSecs,
      priceOffsetPct: v.priceOffsetPct,
      maxTotal: v.maxTotal === "" ? null : v.maxTotal,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/dca-portfolio/start", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!r.ok) {
      const j = (await r.json().catch(() => ({}))) as { error?: string };
      throw new Error(j.error ?? `HTTP ${r.status}`);
    }
    refresh();
  };
  const stop = async () => { await fetch("/api/dca-portfolio/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/dca-portfolio/reset", { method: "POST" }); refresh(); };
  const jump = async (market: string | undefined, to: number) => {
    await fetch("/api/price/jump", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ market, to }),
    });
  };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => {
    await fetch("/api/price/walk", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(patch),
    });
  };

  const running = snap.dcaportfolio?.status === "running";
  const hasDp = snap.dcaportfolio !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid dcaportfolio={snap.dcaportfolio} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>DCA Portfolio setup</h2>
                  <DcaPortfolioForm disabled={running} onStart={start} />
                  {hasDp && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools dcaportfolio={snap.dcaportfolio} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Per-market mid · order placements · <span className="demobadge">DEMO</span></h2>
                  <DcaPortfolioChart ticks={snap.ticks} dcaportfolio={snap.dcaportfolio} />
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
              <p>
                <strong>agent-dca-portfolio</strong> — accumulates (or offloads) assets on Canton across
                several markets in parallel, using the exact same DCA logic as <span className="mono">agent-spot-dca</span>
                but fanned out per market. Every <span className="mono">intervalSecs</span> seconds it places
                one limit order per market at <span className="mono">mid × (1 + priceOffsetPct/100)</span>,
                clamped by an optional <span className="mono">maxTotal</span> stop condition.
              </p>
              <p>
                The placement engine (<span className="mono">lib/dcaportfolio-engine.ts</span>) mirrors the Rust
                loop in <span className="mono">crates/agent-dca-portfolio/src/main.rs</span>. Each market runs
                its own GBM mid in the demo — no real orders are sent. Use the demo tools to nudge any market's
                mid up or down and watch how each independent DCA loop reacts.
              </p>
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
