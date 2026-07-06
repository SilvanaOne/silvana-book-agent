"use client";

import { useCallback, useEffect, useState } from "react";
import { GenesisForm } from "./components/GenesisForm";
import { GenesisChart } from "./components/GenesisChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { GenesisState } from "@/lib/genesis-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: GenesisState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/strategy-genesis/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const compile = async (spec: string) => {
    const r = await fetch("/api/strategy-genesis/compile", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ spec }) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const reset = async () => { await fetch("/api/strategy-genesis/reset", { method: "POST" }); refresh(); };

  const loaded = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={loaded} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Spec compiler</h2>
              <GenesisForm onCompile={compile} onReset={reset} />
            </div>
            <div className="card"><h2>Parser rules</h2><DemoTools agent={snap.agent} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Latest compile · <span className="demobadge">DEMO</span></h2><GenesisChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-strategy-genesis</strong> is a Chat-to-Strategy compiler. A short natural-language execution spec (&quot;buy 100 CC-USDC over 1 hour using TWAP&quot;) becomes a valid TOML plan for <span className="mono">agent-algo-order</span>. The output can be piped straight into <span className="mono">agent-algo-order run --plan</span>.</p>
            <p>The parser here is a deterministic keyword matcher — swap in a real LLM (OpenAI, Anthropic) behind the same interface for full natural-language flexibility. The output schema is stable so downstream agents don&apos;t need to change.</p>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
