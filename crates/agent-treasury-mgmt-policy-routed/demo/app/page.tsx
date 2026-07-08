"use client";

import { useCallback, useEffect, useState } from "react";
import { TreasuryForm, type FormValues } from "./components/TreasuryForm";
import { TreasuryChart } from "./components/TreasuryChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { TreasuryState } from "@/lib/treasury-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = { agent: TreasuryState | null; ticks: Tick[]; events: EventEntry[]; walk: { driftPerTick: number; volPerTick: number } };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.005 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/treasury-mgmt-policy-routed/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/treasury-mgmt-policy-routed/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/treasury-mgmt-policy-routed/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/treasury-mgmt-policy-routed/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.agent?.status === "running";
  const hasAgent = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={running} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} walk={snap.walk} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Policy setup</h2>
              <TreasuryForm disabled={running} onStart={start} />
              {hasAgent && (
                <div className="row" style={{ marginTop: 12, gap: 8 }}>
                  {running && <button className="danger" onClick={stop}>Stop</button>}
                  <button className="ghost" onClick={reset}>Reset</button>
                </div>
              )}
            </div>
            <div className="card"><h2>Demo tools</h2><DemoTools agent={snap.agent} onJump={jump} onWalk={walk} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Targets + routing · <span className="demobadge">DEMO</span></h2><TreasuryChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-treasury-mgmt-policy-routed</strong> maintains per-instrument absolute quote-value targets (like <span className="mono">agent-target-allocation</span>) but wraps every candidate trade in a treasury policy: a per-trade ceiling (<span className="mono">max_trade_quote</span>), a rolling 24h cap (<span className="mono">max_daily_trade_quote</span>), and an approval threshold — trades above it get enqueued into a file consumed by <span className="mono">agent-human-approval</span> instead of submitting directly.</p>
            <p>This demo shocks the driver market to push a target off its band. Watch how the resulting leg routes: green DIRECT when small, orange APPROVAL when large, red REFUSED_CAP when the 24h budget is exhausted. The engine (<span className="mono">lib/treasury-engine.ts</span>) mirrors <span className="mono">crates/agent-treasury-mgmt-policy-routed/src/main.rs</span>.</p>
            <ul style={{ paddingLeft: 18 }}>
              <li><a className="mono" style={{ color: "var(--accent)" }} href="https://github.com/SilvanaOne/silvana-book-agent" target="_blank" rel="noopener">github: silvana-book-agent</a></li>
              <li><a className="mono" style={{ color: "var(--accent)" }} href="https://docs.silvana.one" target="_blank" rel="noopener">docs.silvana.one</a></li>
            </ul>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
