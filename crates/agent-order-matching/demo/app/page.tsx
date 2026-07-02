"use client";

import { useCallback, useEffect, useState } from "react";
import { OrderMatchingForm, type FormValues } from "./components/OrderMatchingForm";
import { OrderMatchingChart } from "./components/OrderMatchingChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { OrderMatchingState } from "@/lib/ordermatching-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  ordermatching: OrderMatchingState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ ordermatching: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/order-matching/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      buyTrigger: v.buyTrigger,
      sellTrigger: v.sellTrigger,
      quantity: v.quantity,
      bookSpreadBps: v.bookSpreadBps,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/order-matching/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/order-matching/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/order-matching/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.ordermatching?.status === "monitoring";
  const hasMr = snap.ordermatching !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid ordermatching={snap.ordermatching} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Order Matching setup</h2>
                  <OrderMatchingForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools ordermatching={snap.ordermatching} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · best bid/offer · triggers · snipes · <span className="demobadge">DEMO</span></h2>
                  <OrderMatchingChart ticks={snap.ticks} ordermatching={snap.ordermatching} />
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
              <p><strong>agent-order-matching</strong> — a trigger sniper. It subscribes to the orderbook depth for one market and watches the top of book. When <span className="mono">best_offer ≤ buy_trigger</span> it places a BID at that offer for <span className="mono">quantity</span> — you are willing to buy at or below the trigger. When <span className="mono">best_bid ≥ sell_trigger</span> it places an OFFER at that bid. Only one open snipe per side at a time — no stacking while a snipe is still live.</p>
              <p>The placement engine (<span className="mono">lib/ordermatching-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-order-matching/src/main.rs</span>. The demo synthesizes the top of book from the mid using <span className="mono">bookSpreadBps</span>. Prices follow a GBM random walk — no real orders are sent, and fills are simulated whenever the mid crosses a resting snipe.</p>
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
