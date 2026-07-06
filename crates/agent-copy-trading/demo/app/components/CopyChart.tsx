"use client";

import type { CopyState } from "@/lib/copy-engine";

type Props = Readonly<{ agent: CopyState | null }>;

const W = 720, H = 380;

export function CopyChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start mirroring.</div>;
  const mirrors = agent.mirrors.slice(-8).reverse();
  const rejections = agent.rejections.slice(-4).reverse();

  const markets = Object.keys(agent.leaderPosByMarket).slice(0, 4);
  const bars: Array<{ market: string; leader: number; follower: number }> = markets.map((m) => ({
    market: m,
    leader: agent.leaderPosByMarket[m] ?? 0,
    follower: agent.followerPosByMarket[m] ?? 0,
  }));
  const maxAbs = Math.max(1, ...bars.flatMap((b) => [Math.abs(b.leader), Math.abs(b.follower)]));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 400 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* Top: leader vs follower net position bars */}
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        leader vs follower net position · scale {(agent.config.scale * 100).toFixed(0)}%
      </text>
      {bars.length === 0 && <text x={10} y={40} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">No leader orders yet…</text>}
      {bars.map((b, i) => {
        const y = 34 + i * 32;
        const centerX = 260;
        const barWmax = 200;
        const leaderW = (Math.abs(b.leader) / maxAbs) * barWmax;
        const followerW = (Math.abs(b.follower) / maxAbs) * barWmax;
        const leaderX = b.leader >= 0 ? centerX : centerX - leaderW;
        const followerX = b.follower >= 0 ? centerX + barWmax + 30 : centerX + barWmax + 30 - followerW;
        return (
          <g key={b.market}>
            <text x={10} y={y + 12} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{b.market}</text>
            {/* leader bar */}
            <line x1={centerX} x2={centerX} y1={y + 2} y2={y + 22} stroke="var(--border)" strokeWidth={0.6} />
            <rect x={leaderX} y={y + 6} width={leaderW} height={12} fill="var(--accent)" opacity={0.85} />
            <text x={centerX - barWmax - 4} y={y + 15} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">leader</text>
            {/* follower bar */}
            <line x1={centerX + barWmax + 30} x2={centerX + barWmax + 30} y1={y + 2} y2={y + 22} stroke="var(--border)" strokeWidth={0.6} />
            <rect x={followerX} y={y + 6} width={followerW} height={12} fill="var(--pos)" opacity={0.85} />
            <text x={centerX + barWmax * 2 + 34} y={y + 15} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">follower</text>
            {/* numeric labels */}
            <text x={centerX + 4 * (b.leader >= 0 ? 1 : -1) + (b.leader >= 0 ? leaderW : -leaderW - 6)} y={y + 15} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor={b.leader >= 0 ? "start" : "end"}>{b.leader.toFixed(2)}</text>
            <text x={centerX + barWmax + 30 + 4 * (b.follower >= 0 ? 1 : -1) + (b.follower >= 0 ? followerW : -followerW - 6)} y={y + 15} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor={b.follower >= 0 ? "start" : "end"}>{b.follower.toFixed(2)}</text>
          </g>
        );
      })}

      {/* Bottom: recent mirrors + rejections */}
      <text x={10} y={H - 156} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">recent mirrors</text>
      {mirrors.length === 0 && <text x={10} y={H - 134} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">No mirrors yet — all leader orders filtered or refused.</text>}
      {mirrors.map((m, i) => {
        const y = H - 146 + i * 14;
        return (
          <g key={`m-${m.seq}`}>
            <rect x={10} y={y - 4} width={44} height={12} rx={3} fill="var(--bg-card)" stroke="var(--pos)" strokeWidth={1} />
            <text x={32} y={y + 4} fill="var(--pos)" fontSize={9} fontFamily="ui-monospace, monospace" textAnchor="middle">MIR</text>
            <text x={62} y={y + 4} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace">L#{m.leaderSeq} → M#{m.seq} · {m.side} {m.market} · leader {m.leaderQty.toFixed(2)} @ {m.price.toFixed(4)} · mirror {m.mirrorQty.toFixed(2)} ({m.mirrorNotional.toFixed(2)})</text>
          </g>
        );
      })}

      {rejections.length > 0 && (
        <>
          <text x={10} y={H - 40} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">recent refusals</text>
          {rejections.map((r, i) => {
            const y = H - 28 + i * 12;
            return (
              <g key={`r-${r.seq}`}>
                <rect x={10} y={y - 3} width={44} height={10} rx={3} fill="var(--bg-card)" stroke="var(--neg)" strokeWidth={1} />
                <text x={32} y={y + 5} fill="var(--neg)" fontSize={8} fontFamily="ui-monospace, monospace" textAnchor="middle">REF</text>
                <text x={62} y={y + 5} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">L#{r.leaderSeq} · {r.market} {r.side} · {r.reason}</text>
              </g>
            );
          })}
        </>
      )}
    </svg>
  );
}
