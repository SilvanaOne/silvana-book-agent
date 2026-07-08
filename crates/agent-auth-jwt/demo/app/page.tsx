"use client";

import { useCallback, useEffect, useState } from "react";
import { AuthForm, type FormValues } from "./components/AuthForm";
import { AuthChart } from "./components/AuthChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { AuthState } from "@/lib/auth-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  auth: AuthState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

type ActionResult =
  | { kind: "decode"; header: unknown; payload: unknown; signature: string }
  | { kind: "verify"; ok: boolean; reason: string }
  | { kind: "generate"; jti: string; role: string; exp: string }
  | { kind: "error"; message: string };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ auth: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");
  const [result, setResult] = useState<ActionResult | null>(null);

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/auth-jwt/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      role: v.role,
      ttlSecs: Number(v.ttlSecs),
      autoRefreshEnabled: v.autoRefreshEnabled,
      autoRefreshIntervalSecs: Number(v.autoRefreshIntervalSecs),
      startingPrice: Number(v.startingPrice),
    };
    const r = await fetch("/api/auth-jwt/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    setResult(null);
    refresh();
  };
  const stop = async () => { await fetch("/api/auth-jwt/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/auth-jwt/reset", { method: "POST" }); setResult(null); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const genNow = async () => {
    try {
      const r = await fetch("/api/auth-jwt/generate", { method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
      const j = await r.json();
      if (!r.ok) { setResult({ kind: "error", message: j.error ?? `HTTP ${r.status}` }); return; }
      const t = j.token as { payload: { jti: string; role: string; exp: number } };
      setResult({ kind: "generate", jti: t.payload.jti, role: t.payload.role, exp: new Date(t.payload.exp * 1000).toISOString() });
      refresh();
    } catch (e) { setResult({ kind: "error", message: (e as Error).message }); }
  };
  const decodeNow = async () => {
    try {
      const r = await fetch("/api/auth-jwt/decode", { method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
      const j = await r.json();
      if (!r.ok) { setResult({ kind: "error", message: j.error ?? `HTTP ${r.status}` }); return; }
      setResult({ kind: "decode", header: j.header, payload: j.payload, signature: j.signature });
      refresh();
    } catch (e) { setResult({ kind: "error", message: (e as Error).message }); }
  };
  const verifyNow = async () => {
    try {
      const r = await fetch("/api/auth-jwt/verify", { method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
      const j = await r.json();
      if (!r.ok) { setResult({ kind: "error", message: j.error ?? `HTTP ${r.status}` }); return; }
      setResult({ kind: "verify", ok: Boolean(j.ok), reason: String(j.reason ?? "") });
      refresh();
    } catch (e) { setResult({ kind: "error", message: (e as Error).message }); }
  };

  const running = snap.auth?.status === "monitoring";
  const hasAuth = snap.auth !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid auth={snap.auth} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Auth setup</h2>
                  <AuthForm disabled={running} onStart={start} />
                  {hasAuth && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>JWT actions</h2>
                  {!running ? (
                    <div className="muted">Start Auth to enable JWT actions.</div>
                  ) : (
                    <>
                      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
                        <button className="primary" onClick={genNow}>Generate new token</button>
                        <button className="ghost" onClick={decodeNow}>Decode current</button>
                        <button className="ghost" onClick={verifyNow}>Verify current</button>
                      </div>
                      {result && (
                        <div className="mono" style={{ marginTop: 10, fontSize: 12, background: "#0d0d13", padding: 10, borderRadius: 6, maxHeight: 220, overflow: "auto" }}>
                          {result.kind === "error" && <span className="negative">error: {result.message}</span>}
                          {result.kind === "generate" && (
                            <>
                              <div className="accent">generated new token</div>
                              <div>jti: {result.jti}</div>
                              <div>role: {result.role}</div>
                              <div>exp: {result.exp}</div>
                            </>
                          )}
                          {result.kind === "decode" && (
                            <>
                              <div className="accent">decoded</div>
                              <pre style={{ margin: 0 }}>{JSON.stringify({ header: result.header, payload: result.payload, signature: result.signature }, null, 2)}</pre>
                            </>
                          )}
                          {result.kind === "verify" && (
                            <>
                              <div className={result.ok ? "positive" : "negative"}>{result.ok ? "PASS" : "FAIL"}</div>
                              <div>{result.reason}</div>
                            </>
                          )}
                        </div>
                      )}
                    </>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools auth={snap.auth} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>JWT structure · rotation timeline · <span className="demobadge">DEMO</span></h2>
                  <AuthChart auth={snap.auth} />
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
              <p><strong>agent-auth-jwt</strong> — a JWT lifecycle sandbox for the Silvana orderbook. The real CLI (<span className="mono">crates/agent-auth-jwt/src/main.rs</span>) mints self-describing Ed25519 tokens (RFC 8037) and offers <span className="mono">generate</span> / <span className="mono">decode</span> / <span className="mono">verify</span> / <span className="mono">user-id</span>. This demo mirrors the four operations against an in-memory clock so you can watch tokens rotate, expire, and be re-issued as time advances.</p>
              <p>Configure a role and TTL, decide whether auto-refresh should rotate tokens on a cadence, and hit <span className="mono">Start</span>. Then use <span className="mono">Generate new token</span> to force a rotation, <span className="mono">Decode current</span> to inspect header + claims, and <span className="mono">Verify current</span> to confirm the mock ES256 signature is intact and not past exp. Signatures are deterministic mocks — this is a teaching model, not a security tool.</p>
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
