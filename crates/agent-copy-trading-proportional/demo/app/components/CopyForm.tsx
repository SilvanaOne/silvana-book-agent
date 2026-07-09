"use client";

import { useState } from "react";

export type FormValues = {
  leader: string;
  follower: string;
  followerPortfolio: string;
  leaderPortfolio: string;
  maxScale: string;
  markets: string;
  marketPool: string;
  maxLeaderNotional: string;
  maxMirrorNotional: string;
  leaderRatePerSec: string;
  dryRun: boolean;
};

const DEFAULTS: FormValues = {
  leader: "party::alpha",
  follower: "party::me",
  followerPortfolio: "10000",
  leaderPortfolio: "100000",
  maxScale: "1.0",
  markets: "CC-USDC,BTC-USD",
  marketPool: "CC-USDC,BTC-USD,SCAM-USDC",
  maxLeaderNotional: "50000",
  maxMirrorNotional: "500",
  leaderRatePerSec: "2",
  dryRun: false,
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function CopyForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }
  async function submit(e: React.FormEvent) { e.preventDefault(); setErr(null); setBusy(true); try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); } }

  const rawScale = (() => {
    const fp = Number(v.followerPortfolio);
    const lp = Number(v.leaderPortfolio);
    if (!Number.isFinite(fp) || !Number.isFinite(lp) || lp <= 0) return null;
    return fp / lp;
  })();

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>Leader party</label><input value={v.leader} onChange={(e) => upd("leader", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Follower (this party)</label><input value={v.follower} onChange={(e) => upd("follower", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Follower portfolio (quote units)</label><input type="number" step="any" min={0.01} value={v.followerPortfolio} onChange={(e) => upd("followerPortfolio", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Leader portfolio (quote units)</label><input type="number" step="any" min={0.01} value={v.leaderPortfolio} onChange={(e) => upd("leaderPortfolio", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <div className="grid-2">
        <div><label>Max scale cap (upper bound on ratio)</label><input type="number" step="any" min={0.01} value={v.maxScale} onChange={(e) => upd("maxScale", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Leader rate (orders / sec)</label><input type="number" step="any" min={0.05} max={20} value={v.leaderRatePerSec} onChange={(e) => upd("leaderRatePerSec", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      {rawScale !== null && (
        <div className="mono" style={{ fontSize: 12, opacity: 0.7 }}>
          raw scale = follower / leader = {rawScale.toFixed(4)} — effective = min(raw, maxScale)
        </div>
      )}
      <div><label>Market whitelist (empty = mirror all)</label><input value={v.markets} onChange={(e) => upd("markets", e.target.value)} disabled={disabled || busy} /></div>
      <div><label>Leader market pool (what the leader trades)</label><input value={v.marketPool} onChange={(e) => upd("marketPool", e.target.value)} disabled={disabled || busy} /></div>
      <div className="grid-2">
        <div><label>Max leader notional (per trade)</label><input type="number" step="any" value={v.maxLeaderNotional} onChange={(e) => upd("maxLeaderNotional", e.target.value)} disabled={disabled || busy} /></div>
        <div><label>Max mirror notional (per trade)</label><input type="number" step="any" value={v.maxMirrorNotional} onChange={(e) => upd("maxMirrorNotional", e.target.value)} disabled={disabled || busy} /></div>
      </div>
      <label className="row" style={{ gap: 8, alignItems: "center", fontSize: 13.5 }}>
        <input type="checkbox" checked={v.dryRun} onChange={(e) => upd("dryRun", e.target.checked)} disabled={disabled || busy} />
        <span><span className="mono">--dry-run</span> — evaluate + log, but don&apos;t &quot;submit&quot; mirror orders</span>
      </label>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start proportional mirror"}</button>
    </form>
  );
}
