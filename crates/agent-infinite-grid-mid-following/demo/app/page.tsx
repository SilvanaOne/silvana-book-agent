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
      const r = await fetch("/api/infinite-grid-mid-following/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      stepPct: v.stepPct,
      levels: v.levels,
      quantityPerLevel: v.quantityPerLevel,
      refreshSecs: v.refreshSecs,
      driftThresholdPct: v.driftThresholdPct,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/infinite-grid-mid-following/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/infinite-grid-mid-following/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/infinite-grid-mid-following/reset", { method: "POST" }); refresh(); };
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
              <p><strong>agent-infinite-grid-mid-following</strong> — a grid market-maker that follows the mid price with no fixed range. On start it builds a ladder around the current mid: <span className="mono">BID_i = mid × (1 − step_pct × i / 100)</span> and <span className="mono">OFFER_i = mid × (1 + step_pct × i / 100)</span> for <span className="mono">i = 1..levels</span>. Every <span className="mono">refresh_secs</span> it measures drift = <span className="mono">|mid − center| / center × 100</span>; if drift exceeds <span className="mono">drift_threshold_pct</span>, all active levels are cancelled and the ladder is rebuilt around the new mid.</p>
              <p>The placement engine (<span className="mono">lib/infinitegrid-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-infinite-grid-mid-following/src/main.rs</span>. Prices are a GBM random walk — no real orders sent, and fills are simulated whenever the mid crosses a resting level.</p>
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
