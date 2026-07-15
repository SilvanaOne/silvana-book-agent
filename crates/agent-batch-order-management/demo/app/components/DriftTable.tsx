"use client";

import type { DriftAnalysis } from "@/lib/rebalance-engine";

function num(n: number, digits: number): string {
  return n.toLocaleString("en-US", { minimumFractionDigits: digits, maximumFractionDigits: digits });
}
function price(n: number): string {
  const abs = Math.abs(n);
  const digits = abs >= 100 ? 2 : abs >= 1 ? 2 : 4;
  return num(n, digits);
}

type Props = Readonly<{ analysis: DriftAnalysis }>;

export function DriftTable({ analysis }: Props) {
  return (
    <div style={{ overflowX: "auto" }}>
      <table className="table">
        <thead>
          <tr>
            <th>Asset</th>
            <th>Qty</th>
            <th>Price</th>
            <th>Value</th>
            <th>Current</th>
            <th>Target</th>
            <th>Drift</th>
            <th style={{ textAlign: "right" }}>Band</th>
          </tr>
        </thead>
        <tbody>
          {analysis.rows.map((r) => (
            <tr key={r.assetSymbol} className={r.isQuote ? "quote" : ""}>
              <td><span className="sym">{r.assetSymbol}</span>{r.isQuote ? " (quote)" : ""}</td>
              <td>{num(r.qty, r.qty >= 100 ? 2 : 4)}</td>
              <td>{price(r.price)}</td>
              <td>{num(r.value, 2)}</td>
              <td>{(r.currentWeight * 100).toFixed(2)}%</td>
              <td>{(r.targetWeight * 100).toFixed(2)}%</td>
              <td style={{ color: r.inBand ? "var(--text-muted)" : "var(--accent)" }}>
                {r.driftBps >= 0 ? "+" : ""}{Math.round(r.driftBps)} bps
              </td>
              <td style={{ textAlign: "right" }}>
                <span className={`pill ${r.isQuote ? "quote" : r.inBand ? "in" : "breach"}`}>
                  {r.isQuote ? "quote" : r.inBand ? "in band" : "breach"}
                </span>
              </td>
            </tr>
          ))}
          <tr className="totrow">
            <td>NAV</td>
            <td colSpan={2}></td>
            <td>{num(analysis.nav, 2)} {analysis.quoteCurrency}</td>
            <td>100.00%</td>
            <td>100.00%</td>
            <td>{Math.round(analysis.maxAbsDriftBps)} max</td>
            <td style={{ textAlign: "right" }}>{analysis.breaches} breach</td>
          </tr>
        </tbody>
      </table>
    </div>
  );
}
