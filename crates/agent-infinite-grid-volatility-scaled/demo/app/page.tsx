"use client";

import { useCallback, useEffect, useState } from "react";
import { InfiniteGridForm, type FormValues } from "./components/InfiniteGridForm";
import { InfiniteGridChart } from "./components/InfiniteGridChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { InfiniteGridState } from "@/lib/infinitegrid-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  infinitegrid: InfiniteGridState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ infinitegrid: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/infinite-grid-volatility-scaled/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      levels: v.levels,
      quantityPerLevel: v.quantityPerLevel,
      volWindow: v.volWindow,
      stepMultiplier: v.stepMultiplier,
      minStepPct: v.minStepPct,
      maxStepPct: v.maxStepPct,
      refreshSecs: v.refreshSecs,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/infinite-grid-volatility-scaled/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/infinite-grid-volatility-scaled/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/infinite-grid-volatility-scaled/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.infinitegrid?.status === "monitoring";
  const hasMr = snap.infinitegrid !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid infinitegrid={snap.infinitegrid} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Infinite Grid setup</h2>
                  <InfiniteGridForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools infinitegrid={snap.infinitegrid} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · grid ladder · rebuilds · fills · <span className="demobadge">DEMO</span></h2>
                  <InfiniteGridChart ticks={snap.ticks} infinitegrid={snap.infinitegrid} />
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
              <p><strong>agent-infinite-grid-volatility-scaled</strong> — a mid-following grid where <span className="mono">step_pct</span> is derived dynamically from a rolling realized-volatility window rather than fixed by config. Each tick appends <span className="mono">ln(mid_t / mid_{"{t-1}"})</span> to a length-<span className="mono">volWindow</span> ring buffer; sigma is the sample stddev, and <span className="mono">step_pct = clamp(stepMultiplier × sigma × 100, minStepPct, maxStepPct)</span>. Every <span className="mono">refreshSecs</span> the ladder is cancelled and re-quoted around mid with the fresh step. Denser grids in calm markets; wider grids when things churn.</p>
              <p>The placement engine (<span className="mono">lib/infinitegrid-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-infinite-grid-volatility-scaled/src/main.rs</span>. Prices are a GBM random walk — no real orders sent, and fills are simulated whenever the mid crosses a resting level.</p>
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
