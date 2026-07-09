"use client";

import { useCallback, useEffect, useState } from "react";
import { OrderExpiryForm, type FormValues } from "./components/OrderExpiryForm";
import { OrderExpiryChart } from "./components/OrderExpiryChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { OrderExpiryState } from "@/lib/orderexpiry-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  orderexpiry: OrderExpiryState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ orderexpiry: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/order-expiry-price-drift/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      maxDriftPct: v.maxDriftPct,
      checkIntervalSecs: v.checkIntervalSecs,
      orderArrivalPerTick: v.orderArrivalPerTick,
      dryRun: v.dryRun,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/order-expiry-price-drift/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/order-expiry-price-drift/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/order-expiry-price-drift/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.orderexpiry?.status === "monitoring";
  const hasOe = snap.orderexpiry !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid orderexpiry={snap.orderexpiry} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Order Expiry setup</h2>
                  <OrderExpiryForm disabled={running} onStart={start} />
                  {hasOe && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools orderexpiry={snap.orderexpiry} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Price + orders vs drift envelope · <span className="demobadge">DEMO</span></h2>
                  <OrderExpiryChart ticks={snap.ticks} orderexpiry={snap.orderexpiry} />
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
              <p><strong>agent-order-expiry-price-drift</strong> — periodically sweeps the agent&apos;s own active orders and cancels any whose resting price has drifted away from the current mid by more than <span className="mono">max-drift-pct</span>. No two-phase signing is needed — just <span className="mono">OrderbookService.GetOrders</span>, <span className="mono">PricingService.GetPrice</span>, and <span className="mono">CancelOrder</span>.</p>
              <p>The demo simulates a live stream of own-orders (Poisson arrivals with mean λ per tick), each posted near the current mid. As the synthetic mid walks over time, orders originally close to fair value can end up outside the drift envelope. The dashed band on the chart shows <em>mid ± max-drift</em>: any BID (green) or OFFER (red) marker that falls outside the band becomes a cancel candidate on the next sweep. The red × marks the moment the sweep cancels it. With <span className="mono">--dry-run</span> the sweep only logs what it would cancel.</p>
              <p>Contrast this with the <span className="mono">TTL</span> variant, which cancels purely on age. A price-drift agent keeps well-priced orders on the book indefinitely and pulls the ones that have gone stale in <em>price</em> — regardless of how long they&apos;ve been resting.</p>
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
