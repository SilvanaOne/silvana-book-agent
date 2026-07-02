"use client";

import type { BlockedPartyState, Settlement } from "@/lib/blockedparty-engine";

type Props = Readonly<{ blockedparty: BlockedPartyState | null }>;

function fmtNotional(n: number): string {
  if (n >= 1000) return n.toFixed(0);
  if (n >= 1) return n.toFixed(2);
  return n.toFixed(4);
}

function timeShort(t: number): string {
  const d = new Date(t);
  const hh = d.getHours().toString().padStart(2, "0");
  const mm = d.getMinutes().toString().padStart(2, "0");
  const ss = d.getSeconds().toString().padStart(2, "0");
  return `${hh}:${mm}:${ss}`;
}

export function BlockedPartyChart({ blockedparty }: Props) {
  if (!blockedparty || blockedparty.settlements.length === 0) {
    return (
      <div style={{ height: 320, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No settlements yet — start the agent to watch the settlement stream.
      </div>
    );
  }

  const blocked = new Set(blockedparty.blocklist);
  const recent: Settlement[] = blockedparty.settlements.slice(-15).reverse();
  const { total, cleared, blocked: blockedCount } = blockedparty.stats;
  const denom = Math.max(1, cleared + blockedCount);
  const clearedPct = (cleared / denom) * 100;
  const blockedPct = (blockedCount / denom) * 100;

  return (
    <div className="stack" style={{ gap: 14 }}>
      <div>
        <div className="row" style={{ justifyContent: "space-between", marginBottom: 6, fontSize: 12, color: "var(--text-faint)" }}>
          <span>Cleared vs blocked (of {total} total settlements)</span>
          <span className="mono">
            <span className="positive">{cleared}</span> · <span className="negative">{blockedCount}</span>
          </span>
        </div>
        <div style={{ display: "flex", height: 24, borderRadius: 4, overflow: "hidden", border: "1px solid var(--border)" }}>
          <div
            title={`Cleared: ${cleared} (${clearedPct.toFixed(1)}%)`}
            style={{ width: `${clearedPct}%`, background: "var(--pos)", transition: "width 0.4s ease" }}
          />
          <div
            title={`Blocked: ${blockedCount} (${blockedPct.toFixed(1)}%)`}
            style={{ width: `${blockedPct}%`, background: "var(--neg)", transition: "width 0.4s ease" }}
          />
        </div>
        <div className="row" style={{ justifyContent: "space-between", marginTop: 4, fontSize: 11, color: "var(--text-faint)" }}>
          <span>{clearedPct.toFixed(1)}% cleared</span>
          <span>{blockedPct.toFixed(1)}% blocked</span>
        </div>
      </div>

      <div style={{ overflowX: "auto" }}>
        <table className="mono" style={{ width: "100%", fontSize: 12, borderCollapse: "collapse" }}>
          <thead>
            <tr style={{ color: "var(--text-faint)", textAlign: "left" }}>
              <th style={{ padding: "4px 6px" }}>time</th>
              <th style={{ padding: "4px 6px" }}>#id</th>
              <th style={{ padding: "4px 6px" }}>buyer</th>
              <th style={{ padding: "4px 6px" }}>seller</th>
              <th style={{ padding: "4px 6px" }}>market</th>
              <th style={{ padding: "4px 6px", textAlign: "right" }}>notional</th>
              <th style={{ padding: "4px 6px" }}>status</th>
            </tr>
          </thead>
          <tbody>
            {recent.map((s) => {
              const buyerHit = blocked.has(s.buyer);
              const sellerHit = blocked.has(s.seller);
              const isBlocked = buyerHit || sellerHit;
              return (
                <tr key={s.id} style={{ borderTop: "1px solid var(--border)" }}>
                  <td style={{ padding: "4px 6px", color: "var(--text-faint)" }}>{timeShort(s.t)}</td>
                  <td style={{ padding: "4px 6px" }}>{s.id}</td>
                  <td style={{ padding: "4px 6px" }} className={buyerHit ? "negative" : ""}>{s.buyer}</td>
                  <td style={{ padding: "4px 6px" }} className={sellerHit ? "negative" : ""}>{s.seller}</td>
                  <td style={{ padding: "4px 6px" }}>{s.market}</td>
                  <td style={{ padding: "4px 6px", textAlign: "right" }}>{fmtNotional(s.notional)}</td>
                  <td style={{ padding: "4px 6px" }} className={isBlocked ? "negative" : "positive"}>
                    {isBlocked
                      ? `✗ blocked (${buyerHit && sellerHit ? "both" : buyerHit ? "buyer" : "seller"})`
                      : "✓ cleared"}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
