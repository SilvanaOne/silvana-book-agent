"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";

import { DEV_PORTFOLIO_ID } from "@/lib/dev-portfolio";
import { withBasePath } from "@/lib/base-path";
import { InfoTip } from "@/app/components/InfoTip";
import { PortfolioCharts } from "@/app/portfolio/portfolio-charts";
import { TargetsEditor } from "@/app/portfolio/targets-editor";

const DEFAULT_POLL_MS = 5000;

type DriftRow = {
  assetSymbol: string;
  targetWeight: string;
  currentWeight: string;
  driftWeightBps: string;
  skippedDueToThreshold: boolean;
};

export type PortfolioOverviewData = {
  portfolioId: string;
  baseCurrency: string;
  targets: Array<{
    assetSymbol: string;
    targetWeight: string;
    minWeight: string | null;
    maxWeight: string | null;
    enabled: boolean;
  }>;
  positions: Array<{ assetSymbol: string; qty: string; value: string; price: string }>;
  drift: DriftRow[];
  currentWeights: Array<{ assetSymbol: string; weight: string }>;
  nav: string | null;
  quoteCurrency: string;
  driftBps: number;
};

function bpsToWeight(bps: string | number): string {
  const n = typeof bps === "number" ? bps : Number(bps);
  if (!Number.isFinite(n)) return String(bps);
  return (n / 10000).toFixed(6);
}

export function PortfolioLive(props: Readonly<{ initialData: PortfolioOverviewData; pollMs?: number }>) {
  const pollMs = props.pollMs ?? DEFAULT_POLL_MS;
  const [data, setData] = useState<PortfolioOverviewData>(props.initialData);
  const [updatedAt, setUpdatedAt] = useState<number>(Date.now());
  const [now, setNow] = useState<number>(Date.now());
  const [pollErr, setPollErr] = useState<string | null>(null);
  const portfolioIdRef = useRef(props.initialData.portfolioId);

  useEffect(() => {
    portfolioIdRef.current = data.portfolioId;
  }, [data.portfolioId]);

  // Sync local state when the server-rendered initialData changes (e.g. after router.refresh() from TargetsEditor Save).
  useEffect(() => {
    setData(props.initialData);
    setUpdatedAt(Date.now());
  }, [props.initialData]);

  useEffect(() => {
    let cancelled = false;

    async function tick() {
      try {
        const res = await fetch(
          withBasePath(`/api/backend/portfolio/${encodeURIComponent(portfolioIdRef.current)}`),
          { cache: "no-store" },
        );
        if (!res.ok) {
          if (!cancelled) setPollErr(`HTTP ${res.status}`);
          return;
        }
        const json = (await res.json()) as PortfolioOverviewData;
        if (!cancelled) {
          setData(json);
          setUpdatedAt(Date.now());
          setPollErr(null);
        }
      } catch (e) {
        if (!cancelled) setPollErr(e instanceof Error ? e.message : String(e));
      }
    }

    const dataTimer = setInterval(tick, pollMs);
    const tickTimer = setInterval(() => setNow(Date.now()), 1000);
    return () => {
      cancelled = true;
      clearInterval(dataTimer);
      clearInterval(tickTimer);
    };
  }, [pollMs]);

  const ageSec = Math.max(0, Math.round((now - updatedAt) / 1000));
  const cashRow = data.currentWeights.find((w) => w.assetSymbol === data.quoteCurrency);

  return (
    <main>
      <h1>Portfolio overview</h1>

      <p>
        <InfoTip text="Portfolio UUID. Used as the identifier across all rebalance APIs and commands. Passed in the URL via ?id=.">
          ID
        </InfoTip>{" "}
        · <code>{data.portfolioId}</code>
        {" · "}
        seed:{" "}
        <Link href={`/portfolio?id=${encodeURIComponent(DEV_PORTFOLIO_ID)}`}>{DEV_PORTFOLIO_ID}</Link>
      </p>

      <p className="muted" style={{ fontSize: "0.85em" }}>
        <InfoTip text={`The page fetches fresh portfolio data every ${pollMs / 1000} seconds in the background. After Drift Imitator or Edit Targets — wait up to ${pollMs / 1000}s for charts to refresh.`}>
          Auto-refresh
        </InfoTip>
        : every {pollMs / 1000}s · last update {ageSec}s ago
        {pollErr ? <span className="err"> · poll error: {pollErr}</span> : null}
      </p>

      {!data.nav ? (
        <p>Not enough data to value the portfolio (no snapshots or active targets).</p>
      ) : (
        <p>
          <InfoTip text="What the portfolio is worth right now, in the quote currency. Computed as the sum of «quantity × price» from the latest price snapshots.">
            Portfolio value
          </InfoTip>{" "}
          <code>{data.nav}</code> {data.quoteCurrency} ·{" "}
          <InfoTip text="Maximum absolute drift among assets, expressed as a weight delta (0.01 = 1% of portfolio value). Compared against the rebalance threshold from API .env.">
            drift (max abs)
          </InfoTip>{" "}
          ≈ <strong>{bpsToWeight(data.driftBps)}</strong>
        </p>
      )}

      <TargetsEditor portfolioId={data.portfolioId} initial={data.targets} />

      <section>
        <h2>
          <InfoTip text="Mismatch between the current and target portfolio structure. The rebalance plan is built on top of it.">
            Drift
          </InfoTip>
        </h2>

        <p>
          <InfoTip text="The portfolio quote currency. Portfolio value is computed in it, and the residual after rebalance lands in it.">
            Quote asset
          </InfoTip>
          : <code>{data.quoteCurrency}</code>
          {cashRow ? (
            <>
              {" "}
              ·{" "}
              <InfoTip text="Current share of the quote asset (cash) in the portfolio value. A high value typically means «under-invested».">
                quote asset weight
              </InfoTip>{" "}
              ≈ <strong>{cashRow.weight}</strong>
            </>
          ) : null}
        </p>

        <table className="data-table">
          <thead>
            <tr>
              <th>
                <InfoTip text="Asset ticker from an active target.">Asset</InfoTip>
              </th>
              <th>
                <InfoTip text="Target weight of the asset (from the Targets table), a fraction from 0 to 1.">
                  Target
                </InfoTip>
              </th>
              <th>
                <InfoTip text="Actual current share of the asset in the portfolio value, based on the latest quantity and price snapshots.">
                  Actual
                </InfoTip>
              </th>
              <th>
                <InfoTip text="Drift as a weight delta: actual weight − target. Positive — overweight, negative — underweight.">
                  Δ weight
                </InfoTip>
              </th>
              <th>
                <InfoTip text="true — |Δ weight| is below the rebalance threshold, the asset is skipped in the plan. false — the asset participates in the rebalance.">
                  Below threshold
                </InfoTip>
              </th>
            </tr>
          </thead>
          <tbody>
            {data.drift.length === 0 ? (
              <tr>
                <td colSpan={5}>No data.</td>
              </tr>
            ) : (
              data.drift.map((d) => (
                <tr key={d.assetSymbol}>
                  <td>{d.assetSymbol}</td>
                  <td>{d.targetWeight}</td>
                  <td>{d.currentWeight}</td>
                  <td>{bpsToWeight(d.driftWeightBps)}</td>
                  <td>{String(d.skippedDueToThreshold)}</td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </section>

      <section>
        <h2>
          <InfoTip text="Latest known positions per asset. Their quantity and price feed the portfolio value and drift calculation.">
            Snapshots
          </InfoTip>
        </h2>
        <table className="data-table">
          <thead>
            <tr>
              <th>
                <InfoTip text="Asset ticker.">Asset</InfoTip>
              </th>
              <th>
                <InfoTip text="Number of units of the asset in the portfolio as of the latest snapshot.">
                  Qty
                </InfoTip>
              </th>
              <th>
                <InfoTip text="Price of one unit of the asset in the quote currency as of the latest snapshot.">
                  Price
                </InfoTip>
              </th>
              <th>
                <InfoTip text="Position value in the quote currency: quantity × price. The sum of all position values ≈ portfolio value.">
                  Value
                </InfoTip>
              </th>
            </tr>
          </thead>
          <tbody>
            {data.positions.length === 0 ? (
              <tr>
                <td colSpan={4}>Empty</td>
              </tr>
            ) : (
              data.positions.map((pos) => (
                <tr key={pos.assetSymbol}>
                  <td>{pos.assetSymbol}</td>
                  <td>{pos.qty}</td>
                  <td>{pos.price}</td>
                  <td>{pos.value}</td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </section>

      <PortfolioCharts currentWeights={data.currentWeights} drift={data.drift} />

      <p className="muted">
        Next:{" "}
        <Link href={`/rebalance?portfolioId=${encodeURIComponent(data.portfolioId)}`}>Rebalance builder</Link>{" "}
        · <Link href={`/monitor?portfolioId=${encodeURIComponent(data.portfolioId)}`}>Monitor</Link>
      </p>
    </main>
  );
}
