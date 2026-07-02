"use client";

import { useCallback, useEffect, useState } from "react";
import { HumanApprovalForm, type FormValues } from "./components/HumanApprovalForm";
import { HumanApprovalChart } from "./components/HumanApprovalChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { HumanApprovalState } from "@/lib/humanapproval-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  humanapproval: HumanApprovalState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ humanapproval: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/human-approval/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      orderArrivalPerTick: Number(v.orderArrivalPerTick),
      autoApprovalEnabled: v.autoApprovalEnabled,
      autoApprovalThreshold: Number(v.autoApprovalThreshold),
      reviewerName: v.reviewerName,
      startingPrice: Number(v.startingPrice),
    };
    const r = await fetch("/api/human-approval/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/human-approval/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/human-approval/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const approve = async (id: string) => {
    await fetch("/api/human-approval/approve", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ id, by: snap.humanapproval?.config.reviewerName }) });
    refresh();
  };
  const reject = async (id: string) => {
    await fetch("/api/human-approval/reject", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ id, by: snap.humanapproval?.config.reviewerName }) });
    refresh();
  };
  const purge = async () => {
    await fetch("/api/human-approval/purge", { method: "POST" });
    refresh();
  };

  const running = snap.humanapproval?.status === "monitoring";
  const hasHa = snap.humanapproval !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid humanapproval={snap.humanapproval} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Human Approval setup</h2>
                  <HumanApprovalForm disabled={running} onStart={start} />
                  {hasHa && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={purge} disabled={!running}>Purge queue</button>
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools humanapproval={snap.humanapproval} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Approval queue · decisions · <span className="demobadge">DEMO</span></h2>
                  <HumanApprovalChart humanapproval={snap.humanapproval} onApprove={approve} onReject={reject} />
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
              <p><strong>agent-human-approval</strong> — a manual sign-off queue for orders that need a human signature before hitting the Silvana orderbook. Upstream agents (DCA bots, grid market makers, hedgers…) append their orders to a JSONL queue file via <span className="mono">enqueue</span>. An operator then runs <span className="mono">list / approve &lt;id&gt; / reject &lt;id&gt; / purge</span>. Approved entries are signed with the agent&apos;s Ed25519 key and submitted through <span className="mono">OrderbookService.SubmitOrder</span>; rejected entries never touch the book. Optional auto-approval bypasses human sign-off for orders whose notional is under a configured threshold.</p>
              <p>This demo simulates upstream enqueues (Poisson-ish arrival from <span className="mono">dca-bot / spot-grid / hedging</span>) and lets you approve or reject each pending row inline. Nothing is actually signed or sent — the state lives entirely in-memory.</p>
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
