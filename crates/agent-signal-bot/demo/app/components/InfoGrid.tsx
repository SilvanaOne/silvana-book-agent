"use client";

import type { SignalBotState } from "@/lib/signalbot-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ signalbot: SignalBotState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ signalbot, walk }: Props) {
  const c = signalbot?.config;
  const stateLabel = signalbot === null ? "idle" : signalbot.status === "monitoring" ? "tailing" : "stopped";
  const submitted = signalbot?.signalOrders.filter((o) => o.status === "submitted").length ?? 0;
  const lastRef = signalbot?.lastSignalRef ?? "—";
  const nextSignalSecs = (() => {
    if (!c) return "—";
    if (c.signalArrivalPerTick <= 0) return "∞";
    return (1 / c.signalArrivalPerTick).toFixed(1) + "s";
  })();
  const throughput = signalbot ? `${signalbot.signalsCount} sig / ${signalbot.ordersCount} ord` : "—";
  const rejectionPct = signalbot && signalbot.ordersCount > 0
    ? ((signalbot.stats.rejected / signalbot.ordersCount) * 100).toFixed(1) + "%"
    : "—";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">signal-bot</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">cursor file (mock)</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SB CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">signals file</span><span className="v mono" style={{ fontSize: 11 }}>{c.signalsFilePath}</span></div>
            <div className="kv-row"><span className="k">from end</span><span className="v">{c.fromEnd ? "yes" : "no"}</span></div>
            <div className="kv-row"><span className="k">dry-run</span><span className={`v ${c.dryRun ? "accent" : ""}`}>{c.dryRun ? "yes" : "no"}</span></div>
            <div className="kv-row"><span className="k">arrival λ</span><span className="v">{c.signalArrivalPerTick}/tick</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">signals file</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">from end</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">dry-run</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">arrival λ</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(signalbot?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v accent">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">signals in queue</span><span className="v">{submitted}</span></div>
        <div className="kv-row"><span className="k">last signal ref</span><span className="v mono" style={{ fontSize: 11 }}>{lastRef}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">submitted</span><span className="v">{signalbot?.stats.submitted ?? 0}</span></div>
        <div className="kv-row"><span className="k">filled</span><span className="v positive">{signalbot?.stats.filled ?? 0}</span></div>
        <div className="kv-row"><span className="k">rejected</span><span className="v negative">{signalbot?.stats.rejected ?? 0}</span></div>
        <div className="kv-row"><span className="k">cancelled</span><span className="v">{signalbot?.stats.cancelled ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CURSOR</div></div>
        <div className="kv-row"><span className="k">bytes offset</span><span className="v accent">{(signalbot?.cursorBytes ?? 0).toLocaleString()}</span></div>
        <div className="kv-row"><span className="k">signal count</span><span className="v">{signalbot?.signalsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">next signal in</span><span className="v">{nextSignalSecs}</span></div>
        <div className="kv-row"><span className="k">rejection rate</span><span className="v">{rejectionPct}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">THROUGHPUT</div></div>
        <div className="kv-row"><span className="k">signals / orders</span><span className="v">{throughput}</span></div>
        <div className="kv-row"><span className="k">price source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
