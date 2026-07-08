"use client";

import type { BatchOrdersState } from "@/lib/batchorders-engine";
import { meanPrice } from "@/lib/batchorders-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function fmtSecs(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "—";
  const s = Math.floor(ms / 1000);
  const mm = Math.floor(s / 60);
  const ss = s % 60;
  if (mm === 0) return `${ss}s`;
  return `${mm}m ${ss}s`;
}

type Props = Readonly<{ batchorders: BatchOrdersState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ batchorders, walk }: Props) {
  const c = batchorders?.config;
  const s = batchorders?.stats;
  const total = batchorders?.batch.length ?? 0;
  const bids = batchorders?.batch.filter((o) => o.side === "BID").length ?? 0;
  const offers = batchorders?.batch.filter((o) => o.side === "OFFER").length ?? 0;
  const avgPrice = batchorders && total > 0
    ? batchorders.batch.reduce((acc, o) => acc + o.price, 0) / total
    : null;
  const muBid = batchorders ? meanPrice(batchorders.batch, "BID") : null;
  const muOffer = batchorders ? meanPrice(batchorders.batch, "OFFER") : null;
  const uptimeMs = batchorders ? batchorders.lastTickAt - batchorders.startedAt : 0;
  const uptimeSec = Math.max(uptimeMs / 1000, 0.001);
  const submitsPerSec = batchorders ? batchorders.submittedCount / uptimeSec : 0;
  const stateLabel = batchorders?.status ?? "idle";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">batch-orders</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BO CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">orders</span><span className="v">{c.orders.length}</span></div>
            <div className="kv-row"><span className="k">submit rate</span><span className="v accent">{c.submitRatePerTick} / tick</span></div>
            <div className="kv-row"><span className="k">abort on error</span><span className="v">{c.abortOnError ? "yes" : "no"}</span></div>
            <div className="kv-row"><span className="k">failure rate</span><span className="v">{(c.failureRatePerTick * 100).toFixed(1)}%</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">orders</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">submit rate</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">abort on error</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">failure rate</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BATCH SUMMARY</div></div>
        <div className="kv-row"><span className="k">total</span><span className="v">{total}</span></div>
        <div className="kv-row"><span className="k">bids</span><span className="v" style={{ color: "var(--pos)" }}>{bids}{muBid !== null ? `  μ ${fmt(muBid)}` : ""}</span></div>
        <div className="kv-row"><span className="k">offers</span><span className="v" style={{ color: "var(--neg)" }}>{offers}{muOffer !== null ? `  μ ${fmt(muOffer)}` : ""}</span></div>
        <div className="kv-row"><span className="k">avg price</span><span className="v">{fmt(avgPrice)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PROGRESS</div></div>
        <div className="kv-row"><span className="k">pending</span><span className="v">{s?.pending ?? 0}</span></div>
        <div className="kv-row"><span className="k">submitted</span><span className="v">{s?.submitted ?? 0}</span></div>
        <div className="kv-row"><span className="k">filled</span><span className="v accent">{s?.filled ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed</span><span className={`v ${(s?.failed ?? 0) > 0 ? "negative" : ""}`}>{s?.failed ?? 0}</span></div>
        <div className="kv-row"><span className="k">cancelled</span><span className="v">{s?.cancelled ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">THROUGHPUT</div></div>
        <div className="kv-row"><span className="k">submissions/s</span><span className="v">{submitsPerSec.toFixed(2)}</span></div>
        <div className="kv-row"><span className="k">uptime</span><span className="v">{fmtSecs(uptimeMs)}</span></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(batchorders?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">OPERATIONS</div></div>
        <div className="kv-row"><span className="k">cancel-all</span><span className="v">bulk cancel button above</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
