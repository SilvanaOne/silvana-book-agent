"use client";

import { useCallback, useEffect, useState } from "react";
import { CashBufferForm, type FormValues } from "./components/CashBufferForm";
import { CashBufferChart } from "./components/CashBufferChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { CashBufferState } from "@/lib/cashbuffer-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  cashbuffer: CashBufferState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ cashbuffer: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/cash-buffer-push-only/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      minCc: v.minCc,
      maxCc: v.maxCc,
      sinkParty: v.sinkParty,
      checkIntervalSecs: v.checkIntervalSecs,
      incomeRate: v.incomeRate,
      startingBalance: v.startingBalance,
    };
    const r = await fetch("/api/cash-buffer-push-only/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/cash-buffer-push-only/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/cash-buffer-push-only/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.cashbuffer?.status === "monitoring";
  const hasCb = snap.cashbuffer !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid cashbuffer={snap.cashbuffer} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Cash Buffer setup</h2>
                  <CashBufferForm disabled={running} onStart={start} />
                  {hasCb && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools cashbuffer={snap.cashbuffer} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Balance · band · target · pushes · <span className="demobadge">DEMO</span></h2>
                  <CashBufferChart ticks={snap.ticks} cashbuffer={snap.cashbuffer} />
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
              <p><strong>agent-cash-buffer-push-only</strong> — keeps the agent&apos;s unlocked Canton Coin balance inside a target band <span className="mono">[min_cc, max_cc]</span>. Every <span className="mono">check_interval</span> seconds it reads the balance: when it rises above <span className="mono">max_cc</span> the excess (down to <span className="mono">target = (min+max)/2</span>) is pushed to <span className="mono">sink_party</span> via <span className="mono">TransferCc</span>; when it drops below <span className="mono">min_cc</span> a warning is logged — the agent can only PUSH, not PULL, so an operator has to refill manually.</p>
              <p>The engine (<span className="mono">lib/cashbuffer-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-cash-buffer-push-only/src/main.rs</span>. Balance is simulated: every tick adds <span className="mono">income_rate</span> CC (as if fills / rewards accrue), and each check may fire a push. No real transfers are submitted.</p>
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
