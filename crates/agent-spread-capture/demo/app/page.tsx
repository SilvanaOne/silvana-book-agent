"use client";

import { useCallback, useEffect, useState } from "react";
import { SpreadCaptureForm, type FormValues } from "./components/SpreadCaptureForm";
import { SpreadCaptureChart } from "./components/SpreadCaptureChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { SpreadCaptureState } from "@/lib/spreadcapture-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  spreadcapture: SpreadCaptureState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ spreadcapture: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/spread-capture/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      spreadBps: v.spreadBps,
      quantity: v.quantity,
      maxInventory: v.maxInventory,
      refreshSecs: v.refreshSecs,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/spread-capture/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/spread-capture/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/spread-capture/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.spreadcapture?.status === "monitoring";
  const hasMr = snap.spreadcapture !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid spreadcapture={snap.spreadcapture} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Spread Capture setup</h2>
                  <SpreadCaptureForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools spreadcapture={snap.spreadcapture} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · bid · offer · fills · inventory · <span className="demobadge">DEMO</span></h2>
                  <SpreadCaptureChart ticks={snap.ticks} spreadcapture={snap.spreadcapture} />
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
              <p><strong>agent-spread-capture</strong> — a single-level two-sided market maker on Canton. Every <span className="mono">refresh_secs</span> the agent cancels its own live quotes, samples the mid, and re-quotes one BID at <span className="mono">mid × (1 − spread_bps/20000)</span> and one OFFER at <span className="mono">mid × (1 + spread_bps/20000)</span>. Realized profit is the captured spread when both sides get filled. An inventory clamp prevents runaway exposure: if <span className="mono">net_inventory &gt; +max_inventory</span> the BID is skipped; if <span className="mono">net_inventory &lt; −max_inventory</span> the OFFER is skipped.</p>
              <p>The placement engine (<span className="mono">lib/spreadcapture-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-spread-capture/src/main.rs</span>. Prices are a GBM random walk — no real orders are sent. A quote is treated as filled the moment the mid crosses its price; PnL is booked against a rolling weighted-average bid cost basis.</p>
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
