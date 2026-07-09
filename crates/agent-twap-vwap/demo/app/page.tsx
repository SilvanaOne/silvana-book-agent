"use client";

import { useCallback, useEffect, useState } from "react";
import { TwapForm, type FormValues } from "./components/TwapForm";
import { TwapChart } from "./components/TwapChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { TwapState } from "@/lib/twap-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  twap: TwapState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ twap: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/twap-vwap/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const volumeCurve = v.volumeCurvePreset === "custom" ? v.volumeCurveCustom : v.volumeCurvePreset;
    const body = {
      market: v.market,
      side: v.side,
      total: v.total,
      slices: v.slices,
      durationSecs: v.durationSecs,
      priceOffsetPct: v.priceOffsetPct,
      limitPrice: v.limitPrice,
      startingPrice: v.startingPrice,
      volumeCurve,
    };
    const r = await fetch("/api/twap-vwap/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/twap-vwap/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/twap-vwap/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.twap?.status === "monitoring";
  const hasTwap = snap.twap !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid twap={snap.twap} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>VWAP setup</h2>
                  <TwapForm disabled={running} onStart={start} />
                  {hasTwap && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools twap={snap.twap} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · slice schedule · placed orders · <span className="demobadge">DEMO</span></h2>
                  <TwapChart ticks={snap.ticks} twap={snap.twap} />
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
              <p><strong>agent-twap-vwap</strong> — volume-weighted TWAP execution. A large parent order is split across <span className="mono">N</span> equal time slots (same fixed cadence as the Linear sibling — one order every <span className="mono">duration / slices</span> seconds), but the <em>size</em> of each slice is scaled by a normalized <span className="mono">volume curve</span> instead of being flat <span className="mono">total / slices</span>. Each slice&apos;s quantity is <span className="mono">total × weight_i / Σ weight</span>, and its price is <span className="mono">mid × (1 + offset%)</span>, optionally clamped by a worst-acceptable <span className="mono">limit-price</span>: BIDs skip when the computed price would exceed the limit, OFFERs skip when it would fall below.</p>
              <p>Presets available in the form: <span className="mono">u-shaped</span> (heavy open/close, thin mid — classic intraday volume shape), <span className="mono">linear</span> (uniform, degenerates to plain TWAP), <span className="mono">front-loaded</span> (monotonically decreasing weights). A <span className="mono">custom</span> mode accepts a comma list of positive weights (e.g. <span className="mono">1,2,3,2,1</span>) whose length must equal the number of slices — the loop normalizes it to sum to 1.0.</p>
              <p>The placement engine (<span className="mono">lib/twap-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-twap-vwap/src/main.rs</span>. Prices are a GBM random walk — no real orders are sent to Canton.</p>
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
