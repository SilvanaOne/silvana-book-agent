"use client";

import { useCallback, useEffect, useState } from "react";
import { LegalForm, type FormValues } from "./components/LegalForm";
import { LegalChart } from "./components/LegalChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { LegalState } from "@/lib/legal-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: LegalState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/legal-compliance/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 500); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/legal-compliance/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/legal-compliance/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/legal-compliance/reset", { method: "POST" }); refresh(); };

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
              <LegalForm disabled={running} onStart={start} />
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
              <h2>Verdicts + jurisdiction activity · <span className="demobadge">DEMO</span></h2>
              <LegalChart agent={snap.agent} />
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
            <p><strong>agent-legal-compliance</strong> maps parties to jurisdictions, then applies per-jurisdiction rules to live settlement events: <span className="mono">allowed_markets</span> (allowlist), <span className="mono">prohibited_markets</span> (denylist), <span className="mono">max_notional_per_trade</span>, and <span className="mono">prohibited_counterparty_jurisdictions</span> (sanction-style pair block). Emits <span className="mono">legal.violation</span> events per breach.</p>
            <p>Read-only — pair with an enforcement agent (<span className="mono">agent-killswitch</span>, <span className="mono">agent-witnesses</span>) if you need action. This demo runs a synthetic settlement stream over 4 sample jurisdictions. The engine (<span className="mono">lib/legal-engine.ts</span>) mirrors <span className="mono">crates/agent-legal-compliance/src/main.rs</span>.</p>
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
