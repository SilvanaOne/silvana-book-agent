"use client";

import { useCallback, useEffect, useState } from "react";
import { BatchOrdersForm, type FormValues } from "./components/BatchOrdersForm";
import { BatchOrdersChart } from "./components/BatchOrdersChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { BatchOrdersState } from "@/lib/batchorders-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  batchorders: BatchOrdersState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ batchorders: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/batch-orders-bulk-cli/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      ordersJsonl: v.ordersJsonl,
      submitRatePerTick: Number(v.submitRatePerTick),
      abortOnError: v.abortOnError,
      failureRatePerTick: Number(v.failureRatePerTick),
      startingPrice: Number(v.startingPrice),
    };
    const r = await fetch("/api/batch-orders-bulk-cli/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/batch-orders-bulk-cli/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/batch-orders-bulk-cli/reset", { method: "POST" }); refresh(); };
  const cancelAll = async () => { await fetch("/api/batch-orders-bulk-cli/cancel-all", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const status = snap.batchorders?.status;
  const running = status === "submitting" || status === "monitoring";
  const hasBatch = snap.batchorders !== null;
  const canCancel = running;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid batchorders={snap.batchorders} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Batch setup</h2>
                  <BatchOrdersForm disabled={running} onStart={start} />
                  {hasBatch && (
                    <div className="row" style={{ marginTop: 12, gap: 8, flexWrap: "wrap" }}>
                      {canCancel && <button className="danger" onClick={cancelAll}>Cancel all pending</button>}
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools batchorders={snap.batchorders} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Batch progress · mid · orders · <span className="demobadge">DEMO</span></h2>
                  <BatchOrdersChart ticks={snap.ticks} batchorders={snap.batchorders} />
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
              <p><strong>agent-batch-orders-bulk-cli</strong> — bulk submit / cancel CLI for Silvana Book. The Rust binary ships three one-shot commands: <span className="mono">submit-batch --file orders.jsonl</span> drains a JSONL file line-by-line through <span className="mono">OrderbookService.SubmitOrder</span>; <span className="mono">cancel-batch --market ... --side ...</span> fetches this party&rsquo;s active orders and cancels a filtered subset; <span className="mono">cancel-all</span> cancels every one.</p>
              <p>This demo simulates the flow end-to-end. Paste (or edit) a JSONL batch, pick a per-tick submit rate, an artificial failure rate, and optionally enable <span className="mono">--abort-on-error</span>. Each tick the engine drains up to N pending → submitted, samples a random fault against your failure rate, and marks any resting <span className="mono">submitted</span> order as filled whenever the simulated mid crosses it (BID at price ≥ mid, OFFER at price ≤ mid). Hit <em>Cancel all pending</em> to fire the <span className="mono">cancel-all</span> path — every non-terminal order flips to <span className="mono">cancelled</span>.</p>
              <p>The engine (<span className="mono">lib/batchorders-engine.ts</span>) is a lightweight port of the state machine in <span className="mono">crates/agent-batch-orders-bulk-cli/src/main.rs</span>. Prices are a GBM random walk — no real orders are sent anywhere.</p>
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
