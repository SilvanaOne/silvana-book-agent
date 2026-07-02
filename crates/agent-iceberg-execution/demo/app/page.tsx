"use client";

import { useCallback, useEffect, useState } from "react";
import { IcebergExecutionForm, type FormValues } from "./components/IcebergExecutionForm";
import { IcebergExecutionChart } from "./components/IcebergExecutionChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { IcebergExecutionState } from "@/lib/icebergexecution-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  icebergexecution: IcebergExecutionState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ icebergexecution: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/iceberg-execution/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      side: v.side,
      total: v.total,
      visible: v.visible,
      price: v.price,
      maxRuntimeSecs: v.maxRuntimeSecs,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/iceberg-execution/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/iceberg-execution/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/iceberg-execution/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.icebergexecution?.status === "monitoring";
  const hasIce = snap.icebergexecution !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid icebergexecution={snap.icebergexecution} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Iceberg setup</h2>
                  <IcebergExecutionForm disabled={running} onStart={start} />
                  {hasIce && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools icebergexecution={snap.icebergexecution} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · limit price · chunks · progress · <span className="demobadge">DEMO</span></h2>
                  <IcebergExecutionChart ticks={snap.ticks} icebergexecution={snap.icebergexecution} />
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
              <p><strong>agent-iceberg-execution</strong> walks a large parent order across the book by exposing only <em>one</em> small child chunk at a time. Instead of TWAP&apos;s time-driven slicing, iceberg is <em>fill-driven</em>: chunk <span className="mono">N+1</span> is placed only after chunk <span className="mono">N</span> has left the active book (filled or cancelled). At any moment the on-book footprint of the parent is ≤ <span className="mono">visible</span>. Runs stop when <span className="mono">total_filled ≥ total</span>, or when <span className="mono">max_runtime_secs</span> is hit.</p>
              <p>The placement engine (<span className="mono">lib/icebergexecution-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-iceberg-execution/src/main.rs</span>. Prices are a GBM random walk — no real orders sent, and fills are simulated whenever the mid crosses the chunk&apos;s limit price.</p>
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
