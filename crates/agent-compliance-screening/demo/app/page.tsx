"use client";

import { useCallback, useEffect, useState } from "react";
import { ComplianceForm, type FormValues } from "./components/ComplianceForm";
import { ComplianceChart } from "./components/ComplianceChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { ComplianceState } from "@/lib/compliance-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: ComplianceState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/compliance-screening/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 500); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = { policy: v.policy, parties: v.parties, markets: v.markets, marketFilter: v.marketFilter, eventRatePerSec: v.eventRatePerSec, emitAccepts: v.emitAccepts };
    const r = await fetch("/api/compliance-screening/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/compliance-screening/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/compliance-screening/reset", { method: "POST" }); refresh(); };

  const running = snap.agent?.status === "running";
  const hasAgent = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={running} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Policy setup</h2>
              <ComplianceForm disabled={running} onStart={start} />
              {hasAgent && (
                <div className="row" style={{ marginTop: 12, gap: 8 }}>
                  {running && <button className="danger" onClick={stop}>Stop</button>}
                  <button className="ghost" onClick={reset}>Reset</button>
                </div>
              )}
            </div>
            <div className="card">
              <h2>Demo tools</h2>
              <DemoTools agent={snap.agent} />
            </div>
          </div>
          <div className="stack">
            <div className="card">
              <h2>Recent verdicts · <span className="demobadge">DEMO</span></h2>
              <ComplianceChart agent={snap.agent} />
            </div>
            <div className="card">
              <h2>Recent events</h2>
              <EventsLog events={snap.events.slice(-10)} />
            </div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-compliance-screening</strong> subscribes to the settlement stream and evaluates every proposal against a TOML policy with four rule kinds: <span className="mono">blocked_pairs</span> (a↔b bidirectional), <span className="mono">party_caps</span> (rolling window daily notional per party), <span className="mono">allowed_counterparties</span> (whitelist), and <span className="mono">blocked_markets</span>. Every settlement gets one <span className="mono">compliance.accept</span> or <span className="mono">compliance.reject</span> event with the list of rule hits. It does not cancel — pair with an enforcement agent for that.</p>
            <p>This demo simulates a synthetic settlement stream over configurable party and market pools. Tighten limits or add a blocked pair to watch the reject count climb. The engine (<span className="mono">lib/compliance-engine.ts</span>) mirrors <span className="mono">crates/agent-compliance-screening/src/main.rs</span>.</p>
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
  </>);
}
