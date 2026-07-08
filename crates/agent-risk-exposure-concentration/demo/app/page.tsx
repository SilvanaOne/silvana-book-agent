"use client";

import { useCallback, useEffect, useState } from "react";
import { RiskExposureForm, type FormValues } from "./components/RiskExposureForm";
import { RiskExposureChart } from "./components/RiskExposureChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { RiskExposureState } from "@/lib/riskexposure-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  riskexposure: RiskExposureState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ riskexposure: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/risk-exposure-concentration/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      markets: v.markets,
      instruments: v.instruments,
      concentrationWarnPct: v.concentrationWarnPct,
      snapshotIntervalSecs: v.snapshotIntervalSecs,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/risk-exposure-concentration/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/risk-exposure-concentration/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/risk-exposure-concentration/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.riskexposure?.status === "monitoring";
  const hasRe = snap.riskexposure !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid riskexposure={snap.riskexposure} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Risk Exposure setup</h2>
                  <RiskExposureForm disabled={running} onStart={start} />
                  {hasRe && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools riskexposure={snap.riskexposure} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Per-instrument concentration · <span className="demobadge">DEMO</span></h2>
                  <RiskExposureChart riskexposure={snap.riskexposure} />
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
              <p><strong>agent-risk-exposure-concentration</strong> — a read-only Tier-3 risk dashboard. Every <span className="mono">snapshotIntervalSecs</span> it walks the party&apos;s balances, prices them at the live mid via each instrument&apos;s <span className="mono">priceMultiplier</span>, and computes portfolio value in quote currency plus the share held by each instrument.</p>
              <p>The dashboard flags <span className="mono">concentration_warn</span> whenever any single instrument&apos;s share exceeds <span className="mono">concentrationWarnPct</span>. Open notional per market and pending-settlement notional are shown alongside so operators can eyeball book-vs-portfolio ratios. No orders are ever placed by this agent.</p>
              <p>The engine (<span className="mono">lib/riskexposure-engine.ts</span>) mirrors <span className="mono">crates/agent-risk-exposure-concentration/src/main.rs</span> (<span className="mono">fn snapshot_once</span>). Prices are a GBM random walk — no real ledger calls, and open notional / pending-settlement figures drift slowly to make the dashboard feel live.</p>
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
