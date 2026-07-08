"use client";

import { useCallback, useEffect, useState } from "react";
import { BlockExecutionForm, type FormValues } from "./components/BlockExecutionForm";
import { BlockExecutionChart } from "./components/BlockExecutionChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { BlockExecutionState } from "@/lib/blockexecution-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  blockexecution: BlockExecutionState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ blockexecution: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");
  const [now, setNow] = useState<number>(() => Date.now());

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/block-execution-twap-iceberg/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(() => { refresh(); setNow(Date.now()); }, 1000);
    return () => clearInterval(id);
  }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      side: v.side,
      total: v.total,
      price: v.price,
      timeSlices: v.timeSlices,
      durationSecs: v.durationSecs,
      visible: v.visible,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/block-execution-twap-iceberg/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/block-execution-twap-iceberg/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/block-execution-twap-iceberg/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.blockexecution?.status === "monitoring";
  const hasBe = snap.blockexecution !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid blockexecution={snap.blockexecution} walk={snap.walk} now={now} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Block Execution setup</h2>
                  <BlockExecutionForm disabled={running} onStart={start} />
                  {hasBe && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools blockexecution={snap.blockexecution} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · limit · slices · chunks · <span className="demobadge">DEMO</span></h2>
                  <BlockExecutionChart ticks={snap.ticks} blockexecution={snap.blockexecution} />
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
              <p><strong>agent-block-execution-twap-iceberg</strong> — a TWAP × Iceberg hybrid that walks a large parent order through the book without leaking its size. The parent quantity is split into <span className="mono">timeSlices</span> equal-time slices. Within each slice, at most <span className="mono">visible</span> quantity is live on the book at the configured limit price. When a chunk fills, the next chunk is posted until the slice quantity is exhausted or its window expires — any unfilled remainder <span className="mono">carries over</span> into the next slice.</p>
              <p>The placement engine (<span className="mono">lib/blockexecution-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-block-execution-twap-iceberg/src/main.rs</span>. Prices are a GBM random walk — no real orders sent. A resting BID is treated as filled when mid ≤ price; a resting OFFER when mid ≥ price.</p>
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
