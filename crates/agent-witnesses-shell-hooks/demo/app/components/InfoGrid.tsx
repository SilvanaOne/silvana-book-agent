"use client";

import type { WitnessesState } from "@/lib/witnesses-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ witnesses: WitnessesState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ witnesses, walk }: Props) {
  const c = witnesses?.config;
  const stats = witnesses?.stats;
  const stateLabel = witnesses === null ? "idle" : witnesses.status === "monitoring" ? "listening" : "stopped";
  const lastInvocation = witnesses && witnesses.invocations.length > 0
    ? witnesses.invocations[witnesses.invocations.length - 1]
    : null;
  const successRate = stats && stats.triggered > 0
    ? ((stats.succeeded / stats.triggered) * 100).toFixed(1) + "%"
    : "—";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">witnesses</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">WT CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">handlers</span><span className="v accent">{c.handlers.length}</span></div>
            <div className="kv-row"><span className="k">market filter</span><span className="v">{c.market || "all"}</span></div>
            <div className="kv-row"><span className="k">arrival / tick</span><span className="v">{c.eventArrivalPerTick}</span></div>
            <div className="kv-row"><span className="k">avg duration</span><span className="v">{c.commandDurationMs} ms</span></div>
            <div className="kv-row"><span className="k">failure rate</span><span className="v">{(c.commandFailureRate * 100).toFixed(1)}%</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">handlers</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">market filter</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">arrival / tick</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">avg duration</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">failure rate</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">HANDLERS</div></div>
        {c && c.handlers.length > 0 ? c.handlers.map((h) => (
          <div className="kv-row" key={h.eventKind}>
            <span className="k mono" style={{ fontSize: 11 }}>{h.eventKind}</span>
            <span className="v mono" style={{ fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={h.command}>
              {h.command} <span className="muted">({stats?.byEventKind[h.eventKind] ?? 0})</span>
            </span>
          </div>
        )) : <div className="kv-row"><span className="k">none</span><span className="v faint">—</span></div>}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">triggered</span><span className="v">{stats?.triggered ?? 0}</span></div>
        <div className="kv-row"><span className="k">succeeded</span><span className="v positive">{stats?.succeeded ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed</span><span className="v negative">{stats?.failed ?? 0}</span></div>
        <div className="kv-row"><span className="k">success rate</span><span className="v accent">{successRate}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LAST INVOCATION</div></div>
        {lastInvocation ? (
          <>
            <div className="kv-row"><span className="k">kind</span><span className="v accent mono" style={{ fontSize: 11 }}>{lastInvocation.eventKind}</span></div>
            <div className="kv-row"><span className="k">command</span><span className="v mono" style={{ fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={lastInvocation.command}>{lastInvocation.command}</span></div>
            <div className="kv-row"><span className="k">exit code</span><span className={`v ${lastInvocation.exitCode === 0 ? "positive" : "negative"}`}>{lastInvocation.exitCode}</span></div>
            <div className="kv-row"><span className="k">duration</span><span className="v">{lastInvocation.durationMs} ms</span></div>
            <div className="kv-row"><span className="k">env keys</span><span className="v mono" style={{ fontSize: 10 }}>{Object.keys(lastInvocation.env).length} vars</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">kind</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">command</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">exit code</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">duration</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">env keys</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(witnesses?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
