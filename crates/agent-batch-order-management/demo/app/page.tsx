"use client";

import { useCallback, useEffect, useState } from "react";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { DriftTable } from "./components/DriftTable";
import { WeightsChart } from "./components/WeightsChart";
import { RebalancePanel } from "./components/RebalancePanel";
import { DemoTools } from "./components/DemoTools";
import { EventsLog } from "./components/EventsLog";
import { Footer } from "./components/Footer";
import type { Snapshot } from "@/lib/store";

async function post(url: string, body?: unknown): Promise<void> {
  const r = await fetch(url, {
    method: "POST",
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) {
    const j = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(j.error ?? `HTTP ${r.status}`);
  }
}

export default function Home() {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/batch-order-management/state", { cache: "no-store" });
      setSnap((await r.json()) as Snapshot);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const preview = async () => { await post("/api/batch-order-management/preview"); await refresh(); };
  const execute = async () => { await post("/api/batch-order-management/execute"); await refresh(); };
  const reset = async () => { await post("/api/batch-order-management/reset"); await refresh(); };
  const drift = async (assetSymbol: string, driftWeight: number) => { await post("/api/batch-order-management/drift", { assetSymbol, driftWeight }); await refresh(); };
  const threshold = async (bps: number) => { await post("/api/batch-order-management/drift", { thresholdBps: bps }); await refresh(); };
  const walk = async (volPerTick: number) => { await post("/api/batch-order-management/drift", { volPerTick }); await refresh(); };

  const jobLive = snap?.job != null && snap.job.phase !== "completed";

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={!!jobLive} />

      <div className="container">
        {tab === "dashboard" && snap && (
          <>
            <InfoGrid snap={snap} />

            <div className="card" style={{ marginBottom: 14 }}>
              <h2>Portfolio drift · <span className="demobadge">DEMO</span></h2>
              <DriftTable analysis={snap.analysis} />
            </div>

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Rebalance flow · <span className="demobadge">DEMO</span></h2>
                  <RebalancePanel
                    analysis={snap.analysis}
                    plan={snap.plan}
                    job={snap.job}
                    onPreview={preview}
                    onExecute={execute}
                  />
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools
                    analysis={snap.analysis}
                    volPerTick={snap.walk.volPerTick}
                    onDrift={drift}
                    onThreshold={threshold}
                    onWalk={walk}
                    onReset={reset}
                  />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Current vs Target weights</h2>
                  <WeightsChart analysis={snap.analysis} />
                </div>
                <div className="card">
                  <h2>Recent events</h2>
                  <EventsLog events={snap.events.slice(-8)} />
                </div>
              </div>
            </div>
          </>
        )}

        {tab === "events" && snap && (
          <>
            <div className="card" style={{ marginBottom: 14 }}>
              <h2>Events log</h2>
              <EventsLog events={snap.events} />
            </div>
            <div className="card">
              <h2>Audit trail · signed checkpoints</h2>
              {snap.audit.length === 0 ? (
                <div className="muted">No audit entries yet.</div>
              ) : (
                <div>
                  {[...snap.audit].reverse().map((a, i) => (
                    <div key={i} className="audit-row">
                      <span className="muted mono" style={{ minWidth: 66 }}>{new Date(a.t).toLocaleTimeString("en-US", { hour12: false })}</span>
                      <span>{a.action}</span>
                      <span className="a-hash">{a.hash.slice(0, 12)}…</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </>
        )}

        {tab === "docs" && (
          <div className="card">
            <h2>About this demo</h2>
            <div style={{ maxWidth: 760, lineHeight: 1.7, fontSize: 13.5 }}>
              <p>
                <strong>agent-batch-order-management</strong> — target-weight portfolio rebalancing over batches of orders.
                Each asset&apos;s <span className="mono">current_weight = market_value / NAV</span> is compared to its configured target;
                the <span className="mono">drift = (target − current) × 10000</span> bps. Any asset whose drift exceeds the
                <span className="mono"> ±threshold</span> is a breach and gets a planned order:
                <span className="mono"> BUY</span> when under-weight, <span className="mono">SELL</span> when over-weight, sized by
                <span className="mono"> |Δweight| × NAV</span>. The quote currency (USDC) is the implicit funding leg.
              </p>
              <p>
                The flow is <span className="mono">preview</span> → <span className="mono">execute</span> → <span className="mono">monitor</span>.
                Executing queues a batch job that the worker picks up (<span className="mono">queued → running → completed</span>), settling one
                transfer per Canton venue with a mock tx hash. On completion the portfolio is snapped to target weights, so drift returns to ~0.
              </p>
              <p>
                Fixtures come straight from <span className="mono">crates/agent-batch-order-management/prisma/seed.ts</span> (the
                &quot;Development portfolio&quot;, NAV ≈ 20406 USDC). The drift math mirrors
                <span className="mono"> packages/portfolio-engine/src/rebalance.ts</span> and the batch/transfer shape mirrors
                <span className="mono"> apps/api/src/routes/portfolio.ts</span> — but everything here runs in-memory with no DB, Redis, or network.
              </p>
              <ul style={{ paddingLeft: 18 }}>
                <li><a className="mono" style={{ color: "var(--accent)" }} href="https://github.com/SilvanaOne/silvana-book-agent" target="_blank" rel="noopener">github: silvana-book-agent</a></li>
                <li><a className="mono" style={{ color: "var(--accent)" }} href="https://docs.silvana.one" target="_blank" rel="noopener">docs.silvana.one</a></li>
                <li><a className="mono" style={{ color: "var(--accent)" }} href="https://canton.network" target="_blank" rel="noopener">canton.network</a></li>
              </ul>
            </div>
          </div>
        )}

        {!snap && tab !== "docs" && <div className="card"><div className="muted">Loading portfolio…</div></div>}
      </div>
      <Footer />
    </>
  );
}
