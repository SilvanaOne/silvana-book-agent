"use client";

import { useCallback, useEffect, useState } from "react";
import { StateMonitorForm, type FormValues } from "./components/StateMonitorForm";
import { StateMonitorChart } from "./components/StateMonitorChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { StateMonitorState } from "@/lib/statemonitor-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  statemonitor: StateMonitorState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ statemonitor: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/state-monitor-self-stream/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      includeOrders: v.includeOrders,
      includeSettlements: v.includeSettlements,
      orderArrivalPerTick: v.orderArrivalPerTick,
      settlementArrivalPerTick: v.settlementArrivalPerTick,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/state-monitor-self-stream/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/state-monitor-self-stream/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/state-monitor-self-stream/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.statemonitor?.status === "monitoring";
  const hasSm = snap.statemonitor !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid statemonitor={snap.statemonitor} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>State Monitor setup</h2>
                  <StateMonitorForm disabled={running} onStart={start} />
                  {hasSm && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools statemonitor={snap.statemonitor} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Event timeline · <span className="demobadge">DEMO</span></h2>
                  <StateMonitorChart statemonitor={snap.statemonitor} />
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
              <p><strong>agent-state-monitor-self-stream</strong> is a read-only observer of this party&apos;s own state on Silvana Book. It subscribes to <span className="mono">SubscribeOrders</span> and <span className="mono">SubscribeSettlements</span> in parallel and prints every event as it arrives — <span className="mono">created</span> / <span className="mono">filled</span> / <span className="mono">cancelled</span> for orders, and <span className="mono">proposal</span> / <span className="mono">settled</span> / <span className="mono">failed</span> for settlements. No ledger writes, no orders placed. Optional <span className="mono">--market</span> filter narrows both streams to a single instrument.</p>
              <p>The demo engine (<span className="mono">lib/statemonitor-engine.ts</span>) mirrors the Rust logging behavior in <span className="mono">crates/agent-state-monitor-self-stream/src/main.rs</span>. Instead of a real gRPC stream, it synthesizes plausible event arrivals: each tick a Poisson-style draw with configurable arrival rates generates a fresh <span className="mono">created</span> / <span className="mono">proposal</span>, and existing open orders / pending proposals resolve into <span className="mono">filled</span>, <span className="mono">cancelled</span>, <span className="mono">settled</span> or <span className="mono">failed</span> follow-ups. The timeline plots events by category across time — no orders are sent anywhere.</p>
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
