"use client";

import { useCallback, useEffect, useState } from "react";
import { DiscloseForm, type FormValues } from "./components/DiscloseForm";
import { DiscloseChart } from "./components/DiscloseChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { DiscloseState } from "@/lib/disclose-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: DiscloseState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/selective-disclosure-filtered/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/selective-disclosure-filtered/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const reset = async () => { await fetch("/api/selective-disclosure-filtered/reset", { method: "POST" }); refresh(); };
  const reload = async () => { await fetch("/api/selective-disclosure-filtered/reload", { method: "POST" }); refresh(); };
  const refilter = async () => { await fetch("/api/selective-disclosure-filtered/refilter", { method: "POST" }); refresh(); };
  const verify = async () => { await fetch("/api/selective-disclosure-filtered/verify", { method: "POST" }); refresh(); };
  const tamper = async (seq: number) => { await fetch("/api/selective-disclosure-filtered/tamper", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ seq }) }); refresh(); };

  const loaded = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={loaded} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Filter setup</h2>
              <DiscloseForm disabled={false} onStart={start} />
              {loaded && (
                <div className="row" style={{ marginTop: 12, gap: 8 }}>
                  <button className="ghost" onClick={reset}>Reset</button>
                </div>
              )}
            </div>
            <div className="card">
              <h2>Demo tools</h2>
              <DemoTools agent={snap.agent} onReload={reload} onReFilter={refilter} onVerify={verify} onTamper={tamper} />
            </div>
          </div>
          <div className="stack">
            <div className="card"><h2>Disclosure log · <span className="demobadge">DEMO</span></h2><DiscloseChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-selective-disclosure-filtered</strong> post-processes an <span className="mono">agent-trading-history</span> JSONL log. Filters (kinds, markets, field redaction) are applied and a fresh hash-chained signed log is emitted — you can share it with an auditor without leaking anything else. The output uses the same record schema as trading-history, so <span className="mono">agent-trading-history verify</span> works on either file.</p>
            <p>This demo generates a synthetic history in-browser and lets you re-run filters + verify the chain. Try &quot;Tamper&quot; to edit any record — the next Verify will detect the broken chain and report the first bad seq. Signatures + hashes in this demo are a faux SHA256 (browser can&apos;t hold a real Ed25519 key); the semantics of chain and verify still hold.</p>
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
