"use client";

import { useCallback, useEffect, useState } from "react";
import { PositionForm, type FormValues } from "./components/PositionForm";
import { PriceChart } from "./components/PriceChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { PositionState } from "@/lib/tpsl-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  position: PositionState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ position: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/tpsl-classic/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 1000);
    return () => clearInterval(id);
  }, [refresh]);

  const start = async (v: FormValues) => {
    const body = { market: v.market, side: v.side, quantity: v.quantity, entryPrice: v.entryPrice,
      tp: v.tp === "" ? null : v.tp, sl: v.sl === "" ? null : v.sl,
      trailingPct: v.trailingPct === "" ? null : v.trailingPct };
    const r = await fetch("/api/tpsl-classic/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/tpsl-classic/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/tpsl-classic/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.position?.status === "monitoring";
  const triggered = snap.position?.status === "triggered";
  const hasPosition = snap.position !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid position={snap.position} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Position setup</h2>
                  <PositionForm disabled={running || triggered} onStart={start} />
                  {hasPosition && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop monitor</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools position={snap.position} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Price chart · <span className="demobadge">DEMO</span></h2>
                  <PriceChart ticks={snap.ticks} position={snap.position} />
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
              <p><strong>agent-tpsl-classic</strong> watches a live orderbook price and fires an exit when Take-Profit or Stop-Loss is hit. Trailing stops ratchet with peak (long) / trough (short).</p>
              <p>The trigger engine (<span className="mono">lib/tpsl-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-tpsl-classic/src/main.rs</span>. Prices are a GBM random walk — no real orders sent.</p>
            </div>
          </div>
        )}
      </div>
      <Footer />
    </>
  );
}
