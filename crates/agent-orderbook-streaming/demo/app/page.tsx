"use client";

import { useCallback, useEffect, useState } from "react";
import { ObStreamForm, type FormValues } from "./components/ObStreamForm";
import { ObStreamChart } from "./components/ObStreamChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { ObStreamState } from "@/lib/obstream-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  agent: ObStreamState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.006 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/orderbook-streaming/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      markets: v.markets,
      depth: v.depth,
      startingPrices: v.startingPrices,
      source: v.source,
      includeOrderbook: v.includeOrderbook,
      includeTrades: v.includeTrades,
      noDepth: v.noDepth,
      noPrices: v.noPrices,
      sinks: v.sinks,
      webhookFailureRate: v.webhookFailureRate,
    };
    const r = await fetch("/api/orderbook-streaming/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/orderbook-streaming/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/orderbook-streaming/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.agent?.status === "streaming";
  const hasAgent = snap.agent !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid agent={snap.agent} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Streaming setup</h2>
                  <ObStreamForm disabled={running} onStart={start} />
                  {hasAgent && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools agent={snap.agent} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Depth ladder → sinks · <span className="demobadge">DEMO</span></h2>
                  <ObStreamChart agent={snap.agent} />
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
              <p><strong>agent-orderbook-streaming</strong> subscribes to <span className="mono">SubscribeOrderbookDepth</span> for every configured market and to <span className="mono">StreamPrices</span> for external mid + best-bid/best-ask ticks. Every update is wrapped as <span className="mono">{`{ kind, ts, market_id, ... }`}</span> and dispatched to one or more sinks: <span className="mono">stdout</span>, an append-only JSONL <span className="mono">file:PATH</span>, and/or an HTTP <span className="mono">webhook:URL</span>. Each sink tracks its own delivered / failed counter.</p>
              <p>This demo simulates both streams — a shared GBM drives per-market mids, from which best-bid/best-ask ladders are rebuilt every tick. Depth snapshots are emitted every {`~`}10 ticks; deltas the rest of the time. Webhook sinks randomly fail at the configured rate so you can watch the pipe turn red and the failure counter climb. The engine (<span className="mono">lib/obstream-engine.ts</span>) mirrors <span className="mono">crates/agent-orderbook-streaming/src/main.rs</span>.</p>
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
