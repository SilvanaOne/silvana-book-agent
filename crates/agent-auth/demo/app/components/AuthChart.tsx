"use client";

import type { AuthState, JwtToken } from "@/lib/auth-engine";

type Props = Readonly<{ auth: AuthState | null }>;

const W = 720;
const BLOCK_H = 108;
const TIMELINE_H = 96;
const H = BLOCK_H + TIMELINE_H + 24;
const PAD_L = 14;
const PAD_R = 14;

export function AuthChart({ auth }: Props) {
  if (!auth || !auth.currentToken) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No token — start Auth to mint a JWT.
      </div>
    );
  }
  const token = auth.currentToken;
  const blockW = (W - PAD_L - PAD_R - 24) / 3;
  const headerText = JSON.stringify(token.header, null, 2);
  const payloadText = JSON.stringify(token.payload, null, 2);
  const sigText = truncate(token.signature, 44);

  // Timeline collects issuedAt + expiresAt from history + current + a "now" pointer.
  const now = Date.now();
  const tokens: JwtToken[] = [...auth.history, token];
  const rawMin = Math.min(now - 60_000, tokens[0]?.issuedAt ?? now);
  const rawMax = Math.max(now + 5_000, tokens[tokens.length - 1]?.expiresAt ?? now);
  const tMin = rawMin;
  const tMax = rawMax;
  const tRange = tMax - tMin || 1;
  const tlY = BLOCK_H + 24 + 12;
  const tlX = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const rowH = 16;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 320 }}>
      {/* header block — red */}
      <g>
        <rect x={PAD_L} y={0} width={blockW} height={BLOCK_H} rx={6} fill="#3a1414" stroke="#ff6060" strokeWidth={1.4} />
        <text x={PAD_L + 8} y={16} fill="#ff9999" fontSize={11} fontFamily="ui-monospace, monospace" fontWeight={700}>HEADER</text>
        <ForeignJson x={PAD_L + 8} y={22} w={blockW - 16} h={BLOCK_H - 30} text={headerText} color="#ffb0b0" />
      </g>
      {/* payload block — purple */}
      <g>
        <rect x={PAD_L + blockW + 12} y={0} width={blockW} height={BLOCK_H} rx={6} fill="#221a3a" stroke="#a58bff" strokeWidth={1.4} />
        <text x={PAD_L + blockW + 20} y={16} fill="#c4b0ff" fontSize={11} fontFamily="ui-monospace, monospace" fontWeight={700}>PAYLOAD</text>
        <ForeignJson x={PAD_L + blockW + 20} y={22} w={blockW - 16} h={BLOCK_H - 30} text={payloadText} color="#d4c4ff" />
      </g>
      {/* signature block — blue */}
      <g>
        <rect x={PAD_L + (blockW + 12) * 2} y={0} width={blockW} height={BLOCK_H} rx={6} fill="#0f2436" stroke="#5cc8ff" strokeWidth={1.4} />
        <text x={PAD_L + (blockW + 12) * 2 + 8} y={16} fill="#a2dcff" fontSize={11} fontFamily="ui-monospace, monospace" fontWeight={700}>SIGNATURE</text>
        <text x={PAD_L + (blockW + 12) * 2 + 8} y={38} fill="#b8e4ff" fontSize={11} fontFamily="ui-monospace, monospace">{sigText}</text>
        <text x={PAD_L + (blockW + 12) * 2 + 8} y={54} fill="#7fb4d8" fontSize={10} fontFamily="ui-monospace, monospace">mock ES256</text>
        <text x={PAD_L + (blockW + 12) * 2 + 8} y={70} fill="#7fb4d8" fontSize={10} fontFamily="ui-monospace, monospace">len {token.signature.length}</text>
        <text x={PAD_L + (blockW + 12) * 2 + 8} y={90} fill={token.verified ? "var(--pos)" : "var(--neg)"} fontSize={10} fontFamily="ui-monospace, monospace">
          {token.verified ? "verified" : "unverified"}
        </text>
      </g>

      {/* timeline */}
      <text x={PAD_L} y={BLOCK_H + 24} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">TIMELINE (issued → expires)</text>
      <line x1={PAD_L} x2={W - PAD_R} y1={tlY + rowH * 1.5} y2={tlY + rowH * 1.5} stroke="#22222c" strokeWidth={1} />

      {tokens.map((tok, i) => {
        const isCurrent = tok === token;
        const y = tlY + rowH * 1.5;
        const x1 = tlX(tok.issuedAt);
        const x2 = tlX(Math.min(tok.expiresAt, tMax));
        const color = isCurrent ? "var(--accent)" : "#5b5b6a";
        return (
          <g key={i}>
            <line x1={x1} x2={x2} y1={y} y2={y} stroke={color} strokeWidth={isCurrent ? 3 : 2} opacity={isCurrent ? 1 : 0.6} />
            <circle cx={x1} cy={y} r={3} fill={color} />
            <circle cx={x2} cy={y} r={3} fill={color} />
            {isCurrent && (
              <>
                <line x1={x1} x2={x1} y1={y - 12} y2={y + 12} stroke="var(--accent)" strokeWidth={1} strokeDasharray="2,2" />
                <text x={x1 + 4} y={y - 14} fill="var(--accent)" fontSize={9} fontFamily="ui-monospace, monospace">issued</text>
                <line x1={x2} x2={x2} y1={y - 12} y2={y + 12} stroke="var(--neg)" strokeWidth={1} strokeDasharray="2,2" />
                <text x={x2 + 4} y={y - 14} fill="var(--neg)" fontSize={9} fontFamily="ui-monospace, monospace">exp</text>
              </>
            )}
          </g>
        );
      })}
      {/* now marker */}
      <g>
        <line x1={tlX(now)} x2={tlX(now)} y1={tlY} y2={tlY + rowH * 3} stroke="#ececf1" strokeWidth={1} strokeDasharray="4,3" />
        <text x={tlX(now) + 4} y={tlY + rowH * 3 - 2} fill="#ececf1" fontSize={9} fontFamily="ui-monospace, monospace">now</text>
      </g>
    </svg>
  );
}

function truncate(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}

function ForeignJson({ x, y, w, h, text, color }: { x: number; y: number; w: number; h: number; text: string; color: string }) {
  // Render JSON as monospace <text> tspans line-by-line so no external HTML is needed.
  const lines = text.split("\n").slice(0, Math.floor(h / 12));
  return (
    <g>
      {lines.map((line, i) => (
        <text key={i} x={x} y={y + 12 + i * 12} fill={color} fontSize={10.5} fontFamily="ui-monospace, monospace" xmlSpace="preserve">
          {truncate(line, Math.floor(w / 6))}
        </text>
      ))}
    </g>
  );
}
