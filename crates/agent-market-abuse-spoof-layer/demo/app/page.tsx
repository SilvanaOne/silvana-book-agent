"use client";

import { useCallback, useEffect, useState } from "react";
import { MarketAbuseForm, type FormValues } from "./components/MarketAbuseForm";
import { MarketAbuseChart } from "./components/MarketAbuseChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { MarketAbuseState } from "@/lib/marketabuse-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  marketabuse: MarketAbuseState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ marketabuse: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/market-abuse-spoof-layer/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      spoofBurst: v.spoofBurst,
      spoofBurstWindowSecs: v.spoofBurstWindowSecs,
      spoofWindowSecs: v.spoofWindowSecs,
      layerMinOrders: v.layerMinOrders,
      layerPriceBandPct: v.layerPriceBandPct,
      orderArrivalPerTick: v.orderArrivalPerTick,
      cancelRatePerTick: v.cancelRatePerTick,
      spoofScenarioEnabled: v.spoofScenarioEnabled,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/market-abuse-spoof-layer/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/market-abuse-spoof-layer/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/market-abuse-spoof-layer/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.marketabuse?.status === "monitoring";
  const hasMr = snap.marketabuse !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid marketabuse={snap.marketabuse} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Market Abuse setup</h2>
                  <MarketAbuseForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools marketabuse={snap.marketabuse} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · order flow · alerts · <span className="demobadge">DEMO</span></h2>
                  <MarketAbuseChart ticks={snap.ticks} marketabuse={snap.marketabuse} />
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
              <p><strong>agent-market-abuse-spoof-layer</strong> — a self-audit that flags two classic patterns in the party&rsquo;s own order flow on Canton: <em>spoofing</em> (a burst of orders cancelled almost immediately) and <em>layering</em> (a stack of same-side orders clustered in a tight price band). Only own orders are visible via the JWT-scoped subscription, so this catches strategy bugs and internal misbehaviour rather than third parties.</p>
              <p>The detection engine (<span className="mono">lib/marketabuse-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-market-abuse-spoof-layer/src/main.rs</span>. Orders arrive with a Poisson rate and are cancelled at a configurable per-tick fraction. With <span className="mono">Spoof scenario</span> enabled a synthetic burst is injected every ~30&thinsp;s so both detectors have something to trip on. No real orders are sent.</p>
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
