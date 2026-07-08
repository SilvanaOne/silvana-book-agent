"use client";

import { useCallback, useEffect, useState } from "react";
import { TestRunForm, type FormValues } from "./components/TestRunForm";
import { TestRunChart } from "./components/TestRunChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { TestRunState } from "@/lib/testrun-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = {
  agent: TestRunState | null;
  events: EventEntry[];
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/test-run-rpc-smoke/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 500); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      partyId: v.partyId,
      endpoint: v.endpoint,
      market: v.market,
      failureRate: v.failureRate,
      latencyMultiplier: v.latencyMultiplier,
      runIntervalSecs: v.runIntervalSecs,
    };
    const r = await fetch("/api/test-run-rpc-smoke/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/test-run-rpc-smoke/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/test-run-rpc-smoke/reset", { method: "POST" }); refresh(); };
  const trigger = async () => { await fetch("/api/test-run-rpc-smoke/trigger", { method: "POST" }); refresh(); };

  const running = snap.agent?.status === "running";
  const hasAgent = snap.agent !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid agent={snap.agent} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Healthcheck setup</h2>
                  <TestRunForm disabled={running} onStart={start} />
                  {hasAgent && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools agent={snap.agent} onTrigger={trigger} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Checks · <span className="demobadge">DEMO</span></h2>
                  <TestRunChart agent={snap.agent} />
                </div>
                <div className="card">
                  <h2>Recent events</h2>
                  <EventsLog events={snap.events.slice(-10)} />
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
              <p><strong>agent-test-run-rpc-smoke</strong> is a read-only pre-flight healthcheck. It exercises every gRPC that any trading agent needs (connect + JWT, <span className="mono">GetMarkets</span>, <span className="mono">GetAllActiveOrders</span>, <span className="mono">GetPendingProposals</span>, <span className="mono">GetMarketData</span>, plus <span className="mono">GetPrice</span> and <span className="mono">GetOrderbookDepth</span> when a market is provided) and reports pass/fail with latency per check. Exits non-zero if any check fails — the exit code hook lets you gate deploys.</p>
              <p>This demo simulates each check with a configurable base latency + per-check failure probability so you can watch an all-green run turn red and back. The engine (<span className="mono">lib/testrun-engine.ts</span>) mirrors <span className="mono">crates/agent-test-run-rpc-smoke/src/main.rs</span>: same check list, same per-check outcome shape.</p>
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
