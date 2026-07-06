"use client";

import { useCallback, useEffect, useState } from "react";
import { ContractForm, type FormValues } from "./components/ContractForm";
import { ContractChart } from "./components/ContractChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { ContractState } from "@/lib/contract-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: ContractState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/contractual-compliance/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 500); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/contractual-compliance/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/contractual-compliance/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/contractual-compliance/reset", { method: "POST" }); refresh(); };

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
              <h2>Contract book</h2>
              <ContractForm disabled={running} onStart={start} />
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
              <h2>Contract windows · <span className="demobadge">DEMO</span></h2>
              <ContractChart agent={snap.agent} />
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
            <p><strong>agent-contractual-compliance</strong> tracks settled flow against a TOML file of bilateral contracts. Each entry declares a counterparty, a market, a rolling <span className="mono">window_hours</span>, and optional <span className="mono">min_notional</span> / <span className="mono">max_notional</span>. As <span className="mono">SETTLED</span> events arrive, notional accrues to any matching contract; on breach the agent emits <span className="mono">contract.under_floor</span> or <span className="mono">contract.over_ceiling</span>; periodic <span className="mono">contract.status</span> events report tallies.</p>
            <p>This demo runs a synthetic settlement stream over configurable party × market pools. The chart shows per-contract tally on a track with min/max thresholds and highlights breaches. The engine (<span className="mono">lib/contract-engine.ts</span>) mirrors <span className="mono">crates/agent-contractual-compliance/src/main.rs</span>.</p>
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
