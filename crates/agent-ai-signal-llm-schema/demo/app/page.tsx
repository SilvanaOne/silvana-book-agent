"use client";

import { useCallback, useEffect, useState } from "react";
import { AiSignalForm, type FormValues } from "./components/AiSignalForm";
import { AiSignalChart } from "./components/AiSignalChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { AiSignalState } from "@/lib/ai-signal-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: AiSignalState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/ai-signal-llm-schema/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/ai-signal-llm-schema/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/ai-signal-llm-schema/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/ai-signal-llm-schema/reset", { method: "POST" }); refresh(); };
  const emit = async (prompt: string) => { await fetch("/api/ai-signal-llm-schema/emit", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ prompt }) }); refresh(); };

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
              <h2>Model setup</h2>
              <AiSignalForm disabled={running} onStart={start} />
              {hasAgent && (
                <div className="row" style={{ marginTop: 12, gap: 8 }}>
                  {running && <button className="danger" onClick={stop}>Stop</button>}
                  <button className="ghost" onClick={reset}>Reset</button>
                </div>
              )}
            </div>
            <div className="card"><h2>Prompt tools</h2><DemoTools agent={snap.agent} onEmit={emit} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Recent AI signals · <span className="demobadge">DEMO</span></h2><AiSignalChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-ai-signal-llm-schema</strong> wraps an LLM in a strict output schema. A short natural-language prompt (&quot;I think CC is going to pump within the hour, strong confidence&quot;) plus a snapshot of the current market context (mids per configured market) become a structured signal: <span className="mono">{`{ market, side, price, quantity, confidence, rationale }`}</span>. Signals below <span className="mono">min_confidence</span> are dropped; the rest are appended to a JSONL that <span className="mono">agent-signal-bot</span> consumes.</p>
            <p>This demo runs a deterministic mock model — same rules as the Rust binary. Replace <span className="mono">mock_llm</span> with a real OpenAI / Anthropic client to go live; the record schema is stable so downstream agents don&apos;t need to change.</p>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
