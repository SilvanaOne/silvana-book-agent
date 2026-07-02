"use client";

import { useCallback, useEffect, useState } from "react";
import { BlockedPartyForm, type FormValues } from "./components/BlockedPartyForm";
import { BlockedPartyChart } from "./components/BlockedPartyChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { BlockedPartyState } from "@/lib/blockedparty-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  blockedparty: BlockedPartyState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ blockedparty: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");
  const [blocklistText, setBlocklistText] = useState<string>("");
  const [reloadBusy, setReloadBusy] = useState(false);
  const [reloadErr, setReloadErr] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/blocked-party/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  // Sync the reload textarea with the runtime blocklist the first time we
  // see one, and any time the user hasn't typed into it yet.
  useEffect(() => {
    if (snap.blockedparty && blocklistText === "") {
      setBlocklistText(snap.blockedparty.blocklist.join("\n"));
    }
  }, [snap.blockedparty, blocklistText]);

  const start = async (v: FormValues) => {
    const body = {
      blocklist: v.blocklist,
      reloadSecs: v.reloadSecs,
      settlementArrivalPerTick: v.settlementArrivalPerTick,
      blockedPartyProbability: v.blockedPartyProbability,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/blocked-party/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    setBlocklistText(v.blocklist);
    refresh();
  };
  const stop = async () => { await fetch("/api/blocked-party/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/blocked-party/reset", { method: "POST" }); setBlocklistText(""); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };
  const reloadBlocklist = async () => {
    setReloadErr(null);
    setReloadBusy(true);
    try {
      const r = await fetch("/api/blocked-party/blocklist", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ blocklist: blocklistText }) });
      if (!r.ok) {
        const j = (await r.json().catch(() => ({}))) as { error?: string };
        throw new Error(j.error ?? `HTTP ${r.status}`);
      }
      refresh();
    } catch (e) {
      setReloadErr((e as Error).message);
    } finally {
      setReloadBusy(false);
    }
  };

  const running = snap.blockedparty?.status === "monitoring";
  const hasBp = snap.blockedparty !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid blockedparty={snap.blockedparty} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Blocked Party setup</h2>
                  <BlockedPartyForm disabled={running} onStart={start} />
                  {hasBp && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                {hasBp && (
                  <div className="card">
                    <h2>Live blocklist</h2>
                    <div className="stack">
                      <label>Edit and reload runtime blocklist</label>
                      <textarea
                        value={blocklistText}
                        onChange={(e) => setBlocklistText(e.target.value)}
                        rows={6}
                        spellCheck={false}
                        disabled={!running || reloadBusy}
                        style={{
                          width: "100%",
                          fontFamily: "ui-monospace, monospace",
                          fontSize: 13,
                          background: "var(--bg-card)",
                          color: "var(--text)",
                          border: "1px solid var(--border)",
                          borderRadius: 6,
                          padding: 8,
                          resize: "vertical",
                        }}
                      />
                      {reloadErr && <div className="negative mono" style={{ fontSize: 13 }}>{reloadErr}</div>}
                      <div className="row" style={{ gap: 8 }}>
                        <button className="primary" onClick={reloadBlocklist} disabled={!running || reloadBusy}>{reloadBusy ? "Reloading…" : "Reload"}</button>
                        <span className="muted" style={{ fontSize: 12 }}>Auto-reload every {snap.blockedparty?.config.reloadSecs ?? "—"}s</span>
                      </div>
                    </div>
                  </div>
                )}
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools blockedparty={snap.blockedparty} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Settlement stream · cleared vs blocked · <span className="demobadge">DEMO</span></h2>
                  <BlockedPartyChart blockedparty={snap.blockedparty} />
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
              <p><strong>agent-blocked-party</strong> streams settlement events from the Silvana orderbook and, for every <span className="mono">SettlementProposal</span>, checks whether the <span className="mono">buyer</span> or <span className="mono">seller</span> party id appears in an operator-maintained blocklist. If either side matches, the agent emits a <span className="mono">compliance.blocked_party_hit</span> record to stdout / JSONL / HTTP webhook — no orders, no cancels, pure compliance screening.</p>
              <p>The blocklist is a plain-text file on disk. The agent re-reads it every <span className="mono">--reload-secs</span> so an operator can add or remove entries live without restarting.</p>
              <p>This UI synthesizes a settlement stream (Poisson-arrival buyer/seller pairs drawn from a party pool, with occasional blocklisted parties spliced in at the configured probability) and applies the same match rule. Edit the blocklist textarea and click <em>Reload</em> to push a new runtime list.</p>
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
