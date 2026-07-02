"use client";

import type { AuthState } from "@/lib/auth-engine";

type Props = Readonly<{ auth: AuthState | null; walk: { driftPerTick: number; volPerTick: number } }>;

function iso(epochMs: number | null | undefined): string {
  if (!epochMs) return "—";
  return new Date(epochMs).toISOString().replace("T", " ").replace("Z", "");
}

export function InfoGrid({ auth, walk }: Props) {
  const c = auth?.config;
  const tok = auth?.currentToken ?? null;
  const now = Date.now();
  const secsToExpiry = tok ? Math.max(0, Math.floor((tok.expiresAt - now) / 1000)) : null;
  const rotatedCount = auth?.history.length ?? 0;
  const stateLabel = auth === null ? "idle" : auth.status === "monitoring" ? (tok ? "armed" : "no-token") : "stopped";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">auth</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">AUTH CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">role</span><span className="v accent">{c.role}</span></div>
            <div className="kv-row"><span className="k">ttl</span><span className="v">{c.ttlSecs}s</span></div>
            <div className="kv-row"><span className="k">auto-refresh</span><span className="v">{c.autoRefreshEnabled ? "on" : "off"}</span></div>
            <div className="kv-row"><span className="k">refresh interval</span><span className="v">{c.autoRefreshIntervalSecs}s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">role</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">ttl</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">auto-refresh</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">refresh interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CURRENT TOKEN</div></div>
        {tok ? (
          <>
            <div className="kv-row"><span className="k">jti</span><span className="v mono">{tok.payload.jti}</span></div>
            <div className="kv-row"><span className="k">role</span><span className="v">{tok.payload.role}</span></div>
            <div className="kv-row"><span className="k">expires</span><span className="v mono">{iso(tok.expiresAt)}</span></div>
            <div className="kv-row"><span className="k">expires in</span><span className={`v ${secsToExpiry !== null && secsToExpiry < 15 ? "negative" : "accent"}`}>{secsToExpiry !== null ? `${secsToExpiry}s` : "—"}</span></div>
            <div className="kv-row"><span className="k">verified</span><span className={`v ${tok.verified ? "positive" : "negative"}`}>{tok.verified ? "yes" : "no"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">jti</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">role</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">expires</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">expires in</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">verified</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">HISTORY</div></div>
        <div className="kv-row"><span className="k">rotated tokens</span><span className="v">{rotatedCount}</span></div>
        <div className="kv-row"><span className="k">last rotation</span><span className="v mono">{iso(auth?.lastRotationAt ?? null)}</span></div>
        <div className="kv-row"><span className="k">buffer cap</span><span className="v">10</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">generated</span><span className="v">{auth?.stats.generated ?? 0}</span></div>
        <div className="kv-row"><span className="k">decoded</span><span className="v">{auth?.stats.decoded ?? 0}</span></div>
        <div className="kv-row"><span className="k">verified</span><span className="v">{auth?.stats.verified ?? 0}</span></div>
        <div className="kv-row"><span className="k">verified ok</span><span className="v accent">{auth?.stats.verifiedOk ?? 0}</span></div>
        <div className="kv-row"><span className="k">expired</span><span className="v">{auth?.stats.expired ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{auth ? auth.currentPrice.toFixed(6) : "—"}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
