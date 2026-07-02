"use client";

import type { TradingHistoryState, HistoryRecord, EventKind } from "@/lib/tradinghistory-engine";

type Props = Readonly<{ tradinghistory: TradingHistoryState | null }>;

const KIND_COLOR: Record<EventKind, string> = {
  "order.created":     "#5b8def",
  "order.filled":      "#6de6a3",
  "order.cancelled":   "#c9a76a",
  "settlement.settled":"#a58cff",
  "settlement.failed": "#ff8dab",
};

const MAX_BLOCKS = 20;
const BLOCK_W = 76;
const BLOCK_H = 66;
const GAP = 22;
const PAD_L = 14;
const PAD_T = 22;
const H = 220;

export function TradingHistoryChart({ tradinghistory }: Props) {
  if (!tradinghistory || tradinghistory.records.length === 0) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No records yet — start Trading History and events will begin chaining here.
      </div>
    );
  }
  const all = tradinghistory.records;
  const startIdx = Math.max(0, all.length - MAX_BLOCKS);
  const shown = all.slice(startIdx);
  const tampered = tradinghistory.tamperedIndex; // absolute index within records[]

  const totalW = PAD_L * 2 + shown.length * BLOCK_W + Math.max(0, shown.length - 1) * GAP;
  const y = PAD_T;

  return (
    <div style={{ overflowX: "auto" }}>
      <svg
        viewBox={`0 0 ${totalW} ${H}`}
        style={{ minWidth: totalW, height: H, maxWidth: "100%" }}
      >
        {/* connector lines: prevHash → hash */}
        {shown.map((rec, i) => {
          if (i === 0) return null;
          const absIdx = startIdx + i;
          const prev = shown[i - 1];
          const x1 = PAD_L + (i - 1) * (BLOCK_W + GAP) + BLOCK_W;
          const x2 = PAD_L + i * (BLOCK_W + GAP);
          const midY = y + BLOCK_H / 2;
          const broken = tampered !== null && absIdx > tampered;
          const linkOK = rec.prevHash === prev.hash;
          const stroke = broken || !linkOK ? "#ff5470" : "#3f5b8a";
          return (
            <g key={`link-${absIdx}`}>
              <line x1={x1} x2={x2} y1={midY} y2={midY} stroke={stroke} strokeWidth={1.5} strokeDasharray={broken ? "4,3" : undefined} />
              <polygon
                points={`${x2 - 6},${midY - 4} ${x2},${midY} ${x2 - 6},${midY + 4}`}
                fill={stroke}
              />
            </g>
          );
        })}

        {shown.map((rec: HistoryRecord, i) => {
          const absIdx = startIdx + i;
          const x = PAD_L + i * (BLOCK_W + GAP);
          const isTampered = rec.tampered === true;
          const isBroken = tampered !== null && absIdx >= tampered;
          const color = KIND_COLOR[rec.kind] ?? "#8a94a6";
          const strokeCol = isTampered ? "#ff5470" : isBroken ? "#ff5470" : color;
          const bgCol = isTampered ? "#3a1420" : "#0e1220";
          return (
            <g key={`block-${absIdx}`}>
              <rect
                x={x}
                y={y}
                width={BLOCK_W}
                height={BLOCK_H}
                fill={bgCol}
                stroke={strokeCol}
                strokeWidth={isTampered || isBroken ? 2 : 1.4}
                rx={5}
              />
              <text x={x + 6} y={y + 14} fill={color} fontSize={10} fontFamily="ui-monospace, monospace" fontWeight={600}>
                #{rec.seq}
              </text>
              <text x={x + BLOCK_W - 6} y={y + 14} fill="var(--text-faint)" fontSize={9} fontFamily="ui-monospace, monospace" textAnchor="end">
                {shortKind(rec.kind)}
              </text>
              <text x={x + BLOCK_W / 2} y={y + 32} fill="#ececf1" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="middle">
                {rec.hash.slice(0, 8)}
              </text>
              <text x={x + BLOCK_W / 2} y={y + 46} fill="var(--text-faint)" fontSize={8} fontFamily="ui-monospace, monospace" textAnchor="middle">
                prev {rec.prevHash === "GENESIS" ? "GENESIS" : rec.prevHash.slice(0, 6)}
              </text>
              <text
                x={x + BLOCK_W / 2}
                y={y + BLOCK_H - 6}
                fill={isTampered ? "#ff5470" : isBroken ? "#ff8dab" : "#6de6a3"}
                fontSize={9}
                fontFamily="ui-monospace, monospace"
                textAnchor="middle"
              >
                {isTampered ? "TAMPERED" : isBroken ? "chain broken" : "verified"}
              </text>
            </g>
          );
        })}

        {/* Global chain-broken banner */}
        {tampered !== null && (
          <g>
            <rect x={PAD_L} y={y + BLOCK_H + 22} width={totalW - PAD_L * 2} height={20} fill="#3a1420" stroke="#ff5470" strokeWidth={1} rx={3} />
            <text x={PAD_L + 10} y={y + BLOCK_H + 36} fill="#ff5470" fontSize={11} fontFamily="ui-monospace, monospace" fontWeight={600}>
              ⚠ CHAIN BROKEN at index {tampered} — all subsequent records fail verification
            </text>
          </g>
        )}
      </svg>
    </div>
  );
}

function shortKind(k: EventKind): string {
  switch (k) {
    case "order.created": return "ord.new";
    case "order.filled": return "ord.fill";
    case "order.cancelled": return "ord.cxl";
    case "settlement.settled": return "sttl.ok";
    case "settlement.failed": return "sttl.err";
  }
}
