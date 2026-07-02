"use client";

import type { SignatureState, SignOperation } from "@/lib/signature-engine";

type Props = Readonly<{ signature: SignatureState | null }>;

const ROW_H = 22;
const HEADER_H = 26;
const W = 720;

const COL_SEQ_W = 44;
const COL_KIND_W = 108;
const COL_MSG_W = 240;
const COL_SIG_W = 220;
const COL_OK_W = 60;

const COL_SEQ_X = 12;
const COL_KIND_X = COL_SEQ_X + COL_SEQ_W;
const COL_MSG_X = COL_KIND_X + COL_KIND_W;
const COL_SIG_X = COL_MSG_X + COL_MSG_W;
const COL_OK_X = COL_SIG_X + COL_SIG_W;

export function SignatureChart({ signature }: Props) {
  if (!signature || signature.operations.length === 0) {
    return (
      <div
        style={{
          height: 240,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          color: "var(--text-faint)",
        }}
      >
        No operations — start the signature agent to see auto sign+verify roundtrips.
      </div>
    );
  }

  // Newest at the top.
  const rows = [...signature.operations].reverse().slice(0, 14);
  const h = HEADER_H + rows.length * ROW_H + 8;

  return (
    <svg viewBox={`0 0 ${W} ${h}`} style={{ width: "100%", height: "auto", maxHeight: 380 }}>
      {/* header */}
      <rect x={0} y={0} width={W} height={HEADER_H} fill="var(--bg-card)" />
      <line x1={0} x2={W} y1={HEADER_H} y2={HEADER_H} stroke="#22222c" strokeWidth={1} />
      <text x={COL_SEQ_X} y={HEADER_H - 8} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">
        #
      </text>
      <text x={COL_KIND_X} y={HEADER_H - 8} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">
        kind
      </text>
      <text x={COL_MSG_X} y={HEADER_H - 8} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">
        message
      </text>
      <text x={COL_SIG_X} y={HEADER_H - 8} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">
        signature / output
      </text>
      <text x={COL_OK_X} y={HEADER_H - 8} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">
        result
      </text>

      {rows.map((op: SignOperation, i: number) => {
        const y0 = HEADER_H + i * ROW_H;
        const yText = y0 + ROW_H - 7;
        const kindColor =
          op.kind === "gen-key"
            ? "var(--accent)"
            : op.kind === "verify"
            ? op.verified
              ? "var(--pos)"
              : "var(--neg)"
            : "#ececf1";
        const displayMsg = truncate(op.input, 36);
        const displayOut = op.kind === "verify" ? op.output : truncate(op.output, 30);
        const okLabel =
          op.kind === "verify" ? (op.verified ? "OK" : "FAIL") : op.kind === "sign-raw" || op.kind === "sign-canonical" ? "signed" : op.kind === "gen-key" ? "new" : "";
        const okColor =
          op.kind === "verify" ? (op.verified ? "var(--pos)" : "var(--neg)") : "var(--text-faint)";

        return (
          <g key={op.seq}>
            {i % 2 === 1 && <rect x={0} y={y0} width={W} height={ROW_H} fill="#161620" />}
            <text
              x={COL_SEQ_X}
              y={yText}
              fill="var(--text-faint)"
              fontSize={11}
              fontFamily="ui-monospace, monospace"
            >
              {op.seq}
            </text>
            <text x={COL_KIND_X} y={yText} fill={kindColor} fontSize={11.5} fontFamily="ui-monospace, monospace">
              {op.kind}
              {op.tampered ? " ⚑" : ""}
            </text>
            <text x={COL_MSG_X} y={yText} fill="#ececf1" fontSize={11.5} fontFamily="ui-monospace, monospace">
              {displayMsg}
            </text>
            <text x={COL_SIG_X} y={yText} fill="#a8a8b3" fontSize={11} fontFamily="ui-monospace, monospace">
              {displayOut}
            </text>
            <text x={COL_OK_X} y={yText} fill={okColor} fontSize={11.5} fontFamily="ui-monospace, monospace">
              {okLabel}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number): string {
  if (s.length <= n) return s;
  return s.slice(0, n - 1) + "…";
}
