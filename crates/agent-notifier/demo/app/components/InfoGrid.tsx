"use client";

import type { NotifierState } from "@/lib/notifier-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ notifier: NotifierState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ notifier, walk }: Props) {
  const c = notifier?.config;
  const streams = c
    ? [c.orders ? "orders" : null, c.settlements ? "settlements" : null, c.prices ? "prices" : null].filter((s): s is string => s !== null).join(",") || "—"
    : "—";
  const total = (notifier?.totalDelivered ?? 0) + (notifier?.totalFailed ?? 0);
  const successRate = total > 0 ? ((notifier!.totalDelivered / total) * 100).toFixed(1) + "%" : "—";
  const rateClass = notifier && total > 0 && notifier.totalFailed / total > 0.1 ? "negative" : "";
  const stateLabel = notifier === null ? "idle" : notifier.status === "monitoring" ? "streaming" : "stopped";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">notifier</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">NT CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">streams</span><span className="v accent">{streams}</span></div>
            <div className="kv-row"><span className="k">sinks</span><span className="v">{c.sinks.length}</span></div>
            <div className="kv-row"><span className="k">market filter</span><span className="v">{c.market ?? "all"}</span></div>
            <div className="kv-row"><span className="k">webhook fail%</span><span className="v">{(c.webhookFailureRate * 100).toFixed(1)}%</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">streams</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">sinks</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">market filter</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">webhook fail%</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRAFFIC</div></div>
        <div className="kv-row"><span className="k">events total</span><span className="v">{notifier?.totalEvents ?? 0}</span></div>
        <div className="kv-row"><span className="k">events / min</span><span className="v">{notifier?.eventsPerMinute ?? 0}</span></div>
        <div className="kv-row"><span className="k">last event</span><span className="v accent">{notifier?.lastEventKind ?? "—"}</span></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(notifier?.currentPrice)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">DELIVERY</div></div>
        <div className="kv-row"><span className="k">delivered</span><span className="v positive">{notifier?.totalDelivered ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed</span><span className={`v ${(notifier?.totalFailed ?? 0) > 0 ? "negative" : ""}`}>{notifier?.totalFailed ?? 0}</span></div>
        <div className="kv-row"><span className="k">success rate</span><span className={`v ${rateClass}`}>{successRate}</span></div>
        <div className="kv-row"><span className="k">throughput</span><span className="v">{total} attempts</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SINKS</div></div>
        {notifier && notifier.sinks.length > 0 ? (
          notifier.sinks.slice(0, 4).map((s) => (
            <div key={s.label} className="kv-row">
              <span className="k mono" style={{ fontSize: 11 }}>{s.label.length > 20 ? s.label.slice(0, 19) + "…" : s.label}</span>
              <span className="v mono" style={{ fontSize: 11 }}>{s.deliveredCount}<span style={{ color: s.failedCount > 0 ? "var(--neg)" : "var(--text-faint)" }}> / {s.failedCount}</span></span>
            </div>
          ))
        ) : (
          <>
            <div className="kv-row"><span className="k">no sinks</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">-</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">-</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">-</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
