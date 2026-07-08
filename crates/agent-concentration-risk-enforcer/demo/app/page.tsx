"use client";

import { useCallback, useEffect, useState } from "react";
import { ConcentrationRiskForm, type FormValues } from "./components/ConcentrationRiskForm";
import { ConcentrationRiskChart } from "./components/ConcentrationRiskChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { ConcentrationRiskState } from "@/lib/concentrationrisk-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  concentrationrisk: ConcentrationRiskState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ concentrationrisk: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/concentration-risk-enforcer/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      instrumentsJson: v.instrumentsJson,
      maxSharePct: v.maxSharePct,
      minSharePct: v.minSharePct,
      checkIntervalSecs: v.checkIntervalSecs,
      dryRun: v.dryRun,
      balanceDriftPerTick: v.balanceDriftPerTick,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/concentration-risk-enforcer/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/concentration-risk-enforcer/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/concentration-risk-enforcer/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.concentrationrisk?.status === "monitoring";
  const hasCr = snap.concentrationrisk !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid concentrationrisk={snap.concentrationrisk} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Concentration Risk setup</h2>
                  <ConcentrationRiskForm disabled={running} onStart={start} />
                  {hasCr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools concentrationrisk={snap.concentrationrisk} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Per-instrument share · thresholds · cancels · <span className="demobadge">DEMO</span></h2>
                  <ConcentrationRiskChart concentrationrisk={snap.concentrationrisk} />
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
              <p><strong>agent-concentration-risk-enforcer</strong> — enforcement variant of <span className="mono">agent-risk-exposure</span>. Each cycle it values the portfolio in quote currency (<span className="mono">balance × mid × priceMultiplier</span>), computes each instrument&apos;s share of portfolio and acts on breaches:</p>
              <ul style={{ paddingLeft: 18 }}>
                <li>share &gt; <span className="mono">max_share_pct</span> → cancel this party&apos;s own <span className="mono">BID</span>s on that instrument&apos;s market (no further accumulation).</li>
                <li>share &lt; <span className="mono">min_share_pct</span> → cancel this party&apos;s own <span className="mono">OFFER</span>s on that market (no further depletion).</li>
                <li><span className="mono">--dry-run</span> logs would-be cancels without touching the book.</li>
              </ul>
              <p>The engine (<span className="mono">lib/concentrationrisk-engine.ts</span>) mirrors the Rust <span className="mono">sweep()</span> in <span className="mono">crates/agent-concentration-risk-enforcer/src/main.rs</span>. Balances random-walk each tick so shares actually drift over time — no real orders sent.</p>
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
