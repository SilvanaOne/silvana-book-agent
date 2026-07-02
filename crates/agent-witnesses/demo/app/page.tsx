"use client";

import { useCallback, useEffect, useState } from "react";
import { WitnessesForm, type FormValues } from "./components/WitnessesForm";
import { WitnessesChart } from "./components/WitnessesChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { WitnessesState } from "@/lib/witnesses-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  witnesses: WitnessesState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

const ENV_VARS: readonly { name: string; desc: string }[] = [
  { name: "SILVANA_EVENT", desc: 'e.g. "settlement.settled", "order.filled"' },
  { name: "SILVANA_EVENT_TS", desc: "ISO-8601 UTC timestamp" },
  { name: "SILVANA_PROPOSAL_ID", desc: "settlement events only" },
  { name: "SILVANA_ORDER_ID", desc: "order events only" },
  { name: "SILVANA_MARKET_ID", desc: 'e.g. "CC-USDC"' },
  { name: "SILVANA_STATUS", desc: "settlement status, e.g. SETTLED" },
];

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ witnesses: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/witnesses/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      handlersText: v.handlersText,
      market: v.market,
      eventArrivalPerTick: v.eventArrivalPerTick,
      commandDurationMs: v.commandDurationMs,
      commandFailureRate: v.commandFailureRate,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/witnesses/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/witnesses/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/witnesses/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.witnesses?.status === "monitoring";
  const hasWt = snap.witnesses !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid witnesses={snap.witnesses} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Witnesses setup</h2>
                  <WitnessesForm disabled={running} onStart={start} />
                  {hasWt && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools witnesses={snap.witnesses} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Event stream · command invocations · <span className="demobadge">DEMO</span></h2>
                  <WitnessesChart witnesses={snap.witnesses} />
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
              <p><strong>agent-witnesses</strong> — event-driven external command launcher. Subscribes to <span className="mono">SubscribeOrders</span> and <span className="mono">SubscribeSettlements</span> from the Silvana orderbook service, and spawns a shell command for every event whose type matches a configured trigger (<span className="mono">--on-settled</span>, <span className="mono">--on-failed</span>, <span className="mono">--on-cancelled</span>, <span className="mono">--on-order-filled</span>, etc). Event metadata is exported to each subprocess via environment variables so trigger scripts can page an on-call, push a webhook, or record a signed audit line without needing to talk to the ledger themselves.</p>
              <p>The demo simulates event arrival (Poisson thin approximation, one event per tick) and mocks command execution with a configurable average duration and failure rate. No real subprocesses are spawned. The engine mirrors the trigger-matching logic of <span className="mono">crates/agent-witnesses/src/main.rs</span>.</p>

              <h3 style={{ marginTop: 18 }}>Environment variables exported to every command</h3>
              <table className="mono" style={{ fontSize: 12, borderCollapse: "collapse", width: "100%", marginTop: 6 }}>
                <thead>
                  <tr style={{ textAlign: "left", color: "var(--text-faint)" }}>
                    <th style={{ padding: "4px 8px 4px 0" }}>variable</th>
                    <th style={{ padding: "4px 0" }}>description</th>
                  </tr>
                </thead>
                <tbody>
                  {ENV_VARS.map((e) => (
                    <tr key={e.name} style={{ borderTop: "1px solid #1c1c26" }}>
                      <td style={{ padding: "4px 8px 4px 0", color: "var(--accent)" }}>{e.name}</td>
                      <td style={{ padding: "4px 0", color: "#b8c5df" }}>{e.desc}</td>
                    </tr>
                  ))}
                </tbody>
              </table>

              <ul style={{ paddingLeft: 18, marginTop: 18 }}>
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
