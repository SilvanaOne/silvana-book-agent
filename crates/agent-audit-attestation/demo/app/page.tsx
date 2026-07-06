"use client";

import { useCallback, useEffect, useState } from "react";
import { AttestForm, type FormValues } from "./components/AttestForm";
import { AttestChart } from "./components/AttestChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { AttestState } from "@/lib/attest-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: AttestState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/audit-attestation/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 500); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/audit-attestation/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/audit-attestation/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/audit-attestation/reset", { method: "POST" }); refresh(); };
  const publish = async () => { await fetch("/api/audit-attestation/publish", { method: "POST" }); refresh(); };

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
              <h2>Attestation setup</h2>
              <AttestForm disabled={running} onStart={start} />
              {hasAgent && (
                <div className="row" style={{ marginTop: 12, gap: 8 }}>
                  {running && <button className="danger" onClick={stop}>Stop</button>}
                  <button className="ghost" onClick={reset}>Reset</button>
                </div>
              )}
            </div>
            <div className="card"><h2>Demo tools</h2><DemoTools agent={snap.agent} onPublish={publish} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Checkpoints · <span className="demobadge">DEMO</span></h2><AttestChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-audit-attestation</strong> reads the current head of an <span className="mono">agent-trading-history</span> JSONL and emits a signed checkpoint carrying <span className="mono">{`{ ts, head_seq, head_hash, party, signature }`}</span>. Auditors can later use any single checkpoint to prove that earlier records existed at that time — without needing the full log.</p>
            <p>This demo simulates a growing trading-history in-browser and publishes checkpoints on the configured interval (or on demand via <span className="mono">Publish checkpoint now</span>). Signatures + hashes are a faux SHA256; the semantics of chain + attestation still hold.</p>
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
