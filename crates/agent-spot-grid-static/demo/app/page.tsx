"use client";

import { useCallback, useEffect, useState } from "react";
import { SpotGridForm, type FormValues } from "./components/SpotGridForm";
import { SpotGridChart } from "./components/SpotGridChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { SpotGridState } from "@/lib/spotgrid-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  spotgrid: SpotGridState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ spotgrid: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/spot-grid-static/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      midPrice: v.midPrice,
      bidLevels: v.bidLevels,
      offerLevels: v.offerLevels,
      stepPct: v.stepPct,
      qtyPerLevel: v.qtyPerLevel,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/spot-grid-static/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/spot-grid-static/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/spot-grid-static/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.spotgrid?.status === "monitoring";
  const hasGrid = snap.spotgrid !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid spotgrid={snap.spotgrid} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Spot Grid setup</h2>
                  <SpotGridForm disabled={running} onStart={start} />
                  {hasGrid && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools spotgrid={snap.spotgrid} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · grid rungs · fills · <span className="demobadge">DEMO</span></h2>
                  <SpotGridChart ticks={snap.ticks} spotgrid={snap.spotgrid} />
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
              <p><strong>agent-spot-grid-static</strong> — minimal passive market-making grid on Canton. On start it stacks <span className="mono">bidLevels</span> BIDs below the mid and <span className="mono">offerLevels</span> OFFERs above, spaced by <span className="mono">stepPct</span>%, each with a fixed <span className="mono">qty/level</span>. As the mid walks through a rung, that order fills — and, unlike <span className="mono">agent-infinite-grid</span>, is NOT restored. No RFQ, no fill loops.</p>
              <p>The placement engine (<span className="mono">lib/spotgrid-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-spot-grid-static/src/main.rs</span>. Prices are a GBM random walk — no real orders sent, and fills are simulated whenever the mid crosses a resting rung.</p>
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
