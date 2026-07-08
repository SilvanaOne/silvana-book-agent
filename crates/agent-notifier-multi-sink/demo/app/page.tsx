"use client";

import { useCallback, useEffect, useState } from "react";
import { NotifierForm, type FormValues } from "./components/NotifierForm";
import { NotifierChart } from "./components/NotifierChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { NotifierState } from "@/lib/notifier-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  notifier: NotifierState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ notifier: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/notifier-multi-sink/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      orders: v.orders,
      settlements: v.settlements,
      prices: v.prices,
      sinks: v.sinks,
      market: v.market,
      webhookFailureRate: v.webhookFailureRate,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/notifier-multi-sink/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/notifier-multi-sink/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/notifier-multi-sink/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.notifier?.status === "monitoring";
  const hasNotifier = snap.notifier !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid notifier={snap.notifier} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Notifier setup</h2>
                  <NotifierForm disabled={running} onStart={start} />
                  {hasNotifier && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools notifier={snap.notifier} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Events → sinks pipeline · <span className="demobadge">DEMO</span></h2>
                  <NotifierChart notifier={snap.notifier} />
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
              <p><strong>agent-notifier-multi-sink</strong> subscribes to <span className="mono">SubscribeOrders</span>, <span className="mono">SubscribeSettlements</span> and <span className="mono">StreamPrices</span>, wraps every event as <span className="mono">{`{ kind, ts, payload }`}</span> and dispatches it to one or more sinks: <span className="mono">stdout</span>, an append-only JSONL <span className="mono">file:PATH</span>, and/or an HTTP <span className="mono">webhook:URL</span>. Each sink tracks its own delivered / failed counter.</p>
              <p>This demo simulates the upstream streams and each sink&apos;s delivery. Webhook sinks randomly fail at the configured rate — you can watch the pipe turn red and the failure counter climb. The engine (<span className="mono">lib/notifier-engine.ts</span>) mirrors the sink model in <span className="mono">crates/agent-notifier-multi-sink/src/main.rs</span>.</p>
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
