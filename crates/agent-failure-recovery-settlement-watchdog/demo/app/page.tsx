"use client";

import { useCallback, useEffect, useState } from "react";
import { FailureRecoveryForm, type FormValues } from "./components/FailureRecoveryForm";
import { FailureRecoveryChart } from "./components/FailureRecoveryChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { FailureRecoveryState } from "@/lib/failurerecovery-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  failurerecovery: FailureRecoveryState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ failurerecovery: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/failure-recovery-settlement-watchdog/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      maxPendingAgeSecs: v.maxPendingAgeSecs,
      checkIntervalSecs: v.checkIntervalSecs,
      cancelRelatedOrders: v.cancelRelatedOrders,
      dryRun: v.dryRun,
      proposalArrivalPerTick: v.proposalArrivalPerTick,
      pendingStuckProbability: v.pendingStuckProbability,
      failureProbability: v.failureProbability,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/failure-recovery-settlement-watchdog/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/failure-recovery-settlement-watchdog/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/failure-recovery-settlement-watchdog/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.failurerecovery?.status === "monitoring";
  const hasFr = snap.failurerecovery !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid failurerecovery={snap.failurerecovery} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Failure Recovery setup</h2>
                  <FailureRecoveryForm disabled={running} onStart={start} />
                  {hasFr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools failurerecovery={snap.failurerecovery} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Proposal lifetimes · sweep events · <span className="demobadge">DEMO</span></h2>
                  <FailureRecoveryChart ticks={snap.ticks} failurerecovery={snap.failurerecovery} />
                </div>
                <div className="card">
                  <h2>Recent events</h2>
                  <EventsLog events={snap.events.slice(-8)} />
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
              <p><strong>agent-failure-recovery-settlement-watchdog</strong> — a watchdog that periodically sweeps this party&apos;s settlement proposals and surfaces problems for an operator to act on. On every sweep it inspects the proposal buffer and flags any <span className="mono">PENDING</span> proposal older than <span className="mono">--max-pending-age-secs</span>, plus any that already transitioned to <span className="mono">FAILED</span>.</p>
              <p>When <span className="mono">--cancel-related-orders</span> is enabled, the sweeper also calls <span className="mono">CancelOrder</span> for any of the agent&apos;s own active orders linked to a stuck / failed proposal (matched via <span className="mono">OrderMatch.bid_order_id</span> / <span className="mono">offer_order_id</span>). With <span className="mono">--dry-run</span> the cancels are logged but not sent. The deep retry logic still lives in <span className="mono">settlement.rs</span> — this agent is a watchdog, not a replacement.</p>
              <p>The demo simulates proposal arrivals (some stuck, some resolving to settled or failed) so you can watch the sweeper flag stale ones and cancel related orders in near-real-time. No live gRPC calls are made — the engine (<span className="mono">lib/failurerecovery-engine.ts</span>) mirrors the Rust sweep logic in <span className="mono">crates/agent-failure-recovery-settlement-watchdog/src/main.rs</span>.</p>
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
