"use client";

import Link from "next/link";
import { useCallback, useMemo, useState } from "react";

import { InfoTip } from "@/app/components/InfoTip";
import { DriftImitator } from "@/app/rebalance/drift-imitator";
import { withBasePath } from "@/lib/base-path";

type PreviewOk = Readonly<{
  previewId: string;
  nav: string;
  warnings: string[];
  drift: ReadonlyArray<{
    assetSymbol: string;
    driftWeightBps: string;
    skippedDueToThreshold: boolean;
  }>;
  plannedOrders: ReadonlyArray<{
    market: string;
    assetSymbol: string;
    side: string;
    qty: string;
    type: string;
    price: string;
  }>;
  estimatedNotional: string;
  riskForLive: { liveExecuteAllowed: boolean; violations: ReadonlyArray<{ code: string; message: string }> };
}>;

function bpsToWeight(bps: string | number): string {
  const n = typeof bps === "number" ? bps : Number(bps);
  if (!Number.isFinite(n)) return String(bps);
  return (n / 10000).toFixed(6);
}

const SEED_TARGETS = `[
  { "assetSymbol": "CC", "weight": 0.4, "enabled": true },
  { "assetSymbol": "USDC", "weight": 0.6, "enabled": true }
]`;

export function RebalanceBuilder(props: Readonly<{ initialPortfolioId: string }>) {
  const [portfolioId, setPortfolioId] = useState(props.initialPortfolioId);
  const [thresholdWeight, setThresholdWeight] = useState("");
  const [targetsJson, setTargetsJson] = useState(SEED_TARGETS);
  const [busy, setBusy] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const [preview, setPreview] = useState<PreviewOk | null>(null);
  const [jobIdResult, setJobIdResult] = useState<{ jobId: string; dryRun?: boolean } | null>(null);

  const bodyBase = useMemo(() => {
    try {
      const trimmed = targetsJson.trim();
      const parsed: unknown = trimmed === "" ? [] : JSON.parse(trimmed);
      if (!Array.isArray(parsed)) throw new Error("targets must be a JSON array");

      const out: Record<string, unknown> = { portfolioId, targets: parsed };

      const t = thresholdWeight.trim();
      if (t.length > 0) {
        const n = Number(t);
        if (!Number.isFinite(n) || n < 0) throw new Error("threshold must be a number ≥ 0");
        out.thresholdBps = Math.round(n * 10000);
      }
      return { ok: true as const, value: out };
    } catch (err) {
      return {
        ok: false as const,
        error: err instanceof Error ? err.message : String(err),
      };
    }
  }, [portfolioId, targetsJson, thresholdWeight]);

  const simulate = useCallback(async () => {
    setLastError(null);
    setJobIdResult(null);
    if (!bodyBase.ok) {
      setLastError(bodyBase.error);
      setPreview(null);
      return;
    }

    setBusy(true);
    try {
      const res = await fetch(withBasePath(`/api/backend/rebalance/preview`), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(bodyBase.value),
      });
      const json: unknown = await res.json();
      if (!res.ok) {
        setPreview(null);
        const msg = typeof json === "object" && json && "message" in json ? String((json as { message?: unknown }).message) : "";
        const code = typeof json === "object" && json && "error" in json ? String((json as { error?: unknown }).error) : "";
        setLastError(msg || code || JSON.stringify(json));
        return;
      }

      const p = json as Partial<PreviewOk>;
      const rlUnknown =
        typeof p.riskForLive === "object" && p.riskForLive !== null
          ? (p.riskForLive as { liveExecuteAllowed?: unknown; violations?: unknown })
          : null;
      if (
        typeof p.previewId !== "string" ||
        typeof p.nav !== "string" ||
        typeof p.estimatedNotional !== "string" ||
        !rlUnknown ||
        typeof rlUnknown.liveExecuteAllowed !== "boolean" ||
        !Array.isArray(rlUnknown.violations) ||
        !Array.isArray(p.plannedOrders) ||
        !Array.isArray(p.drift) ||
        !Array.isArray(p.warnings)
      ) {
        setPreview(null);
        setLastError(`Unexpected preview response: ${JSON.stringify(json)}`);
        return;
      }

      setPreview(p as PreviewOk);
    } catch (e) {
      setPreview(null);
      setLastError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [bodyBase]);

  async function enqueueJob(dryRun: boolean): Promise<void> {
    setLastError(null);
    setJobIdResult(null);
    if (!bodyBase.ok) {
      setLastError(bodyBase.error);
      return;
    }

    setBusy(true);
    try {
      const payload = {
        ...bodyBase.value,
        dryRun,
      };

      const res = await fetch(withBasePath(`/api/backend/rebalance/execute`), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(payload),
      });
      const json: unknown = await res.json();

      if (!res.ok) {
        const msg = typeof json === "object" && json && "message" in json ? String((json as { message?: unknown }).message) : "";
        setLastError(msg || JSON.stringify(json));
        return;
      }

      const id =
        typeof json === "object" && json && "jobId" in json && typeof (json as { jobId: unknown }).jobId === "string" ?
          (json as { jobId: string }).jobId
        : null;

      if (!id) {
        setLastError(`Unexpected enqueue response: ${JSON.stringify(json)}`);
        return;
      }

      setJobIdResult({ jobId: id, dryRun });
      setPreview(null);
    } catch (e) {
      setLastError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const jobHref = jobIdResult ? `/monitor/${encodeURIComponent(jobIdResult.jobId)}` : null;

  return (
    <div className="stack">
      <div className="form-grid">
        <label>
          <InfoTip text="Portfolio UUID. Must match the entry on the Portfolio page. Default is the DEV portfolio from prisma seed.">
            Portfolio ID
          </InfoTip>
          <input value={portfolioId} onChange={(e) => setPortfolioId(e.target.value)} spellCheck={false} />
        </label>

        <label>
          <InfoTip text="Asset drift filtering threshold as a weight delta (0.01 = 1% of portfolio value). Assets with |Δ weight| below this are skipped in the plan. Empty — fall back to the API default.">
            Threshold (weight)
          </InfoTip>
          <input
            value={thresholdWeight}
            onChange={(e) => setThresholdWeight(e.target.value)}
            placeholder="empty = use API default · example: 0.01"
          />
        </label>
      </div>

      <label className="block">
        <InfoTip text="Target portfolio structure: an array of objects { assetSymbol, weight, enabled, minWeight?, maxWeight? }. The sum of weight where enabled=true ≈ 1.0. Empty array [] — take active targets from the DB.">
          Targets JSON
        </InfoTip>{" "}
        (empty array or [] = take active targets from DB)
        <textarea value={targetsJson} rows={10} spellCheck={false} onChange={(e) => setTargetsJson(e.target.value)} />
      </label>

      <div className="btn-row">
        <button type="button" onClick={() => void simulate()} disabled={busy}>
          Simulate (preview)
        </button>
        <InfoTip
          text="Preview of the rebalance result without writing to the DB or enqueueing. Returns the order plan, drift and risk gates summary."
          label="Simulate hint"
          align="start"
        />

        <button type="button" onClick={() => void enqueueJob(true)} disabled={busy}>
          Dry run execution
        </button>
        <InfoTip
          text="Enqueues a job with dryRun=true: the full pipeline (router, risk, batch submit) runs without real venue calls."
          label="Dry run hint"
          align="start"
        />

        <button
          type="button"
          className="danger"
          disabled={busy || !preview?.riskForLive?.liveExecuteAllowed}
          onClick={() => void enqueueJob(false)}
        >
          Live execute
        </button>
        <InfoTip
          text="Real execution against venues. Active only if the latest Simulate confirmed all risk gates pass (liveExecuteAllowed=true)."
          label="Live execute hint"
          align="start"
        />
      </div>

      {!bodyBase.ok ? <p className="err">{bodyBase.error}</p> : null}
      {lastError ? <p className="err">{lastError}</p> : null}

      {preview?.riskForLive ? (
        <p className="muted">
          <InfoTip text="Risk gates summary from the latest Simulate. If 'no' — Live execute is blocked until the listed violations are fixed.">
            Live risks pass
          </InfoTip>
          : <strong>{preview.riskForLive.liveExecuteAllowed ? "yes" : "no"}</strong>
          {!preview.riskForLive.liveExecuteAllowed && preview.riskForLive.violations.length ? (
            <> ({preview.riskForLive.violations.map((v) => v.code).join(", ")})</>
          ) : null}
        </p>
      ) : null}

      {jobHref ? (
        <p>
          Open monitor: <Link href={jobHref}>{jobIdResult?.jobId}</Link>
          {jobIdResult?.dryRun !== undefined ? (
            <>
              {" · "}
              mode <code>{jobIdResult.dryRun ? "dry_run" : "live"}</code>
            </>
          ) : null}
        </p>
      ) : null}

      {preview ? (
        <section>
          <h2>
            <InfoTip text="Snapshot of the latest Simulate. previewId is the calculation id; use it to find the audit log entry and related warnings.">
              Latest preview
            </InfoTip>{" "}
            (#{preview.previewId})
          </h2>
          <p>
            <InfoTip text="What the portfolio is worth right now, in the quote currency, based on the latest quantity and price from snapshots.">
              Portfolio value
            </InfoTip>{" "}
            {preview.nav} ·{" "}
            <InfoTip text="Rough estimate of the plan trading volume: sum of |quantity| × price across plannedOrders. Used by risk gates for daily and per-trade limits.">
              notional
            </InfoTip>
            ≈ <code>{preview.estimatedNotional}</code>
          </p>

          {preview.warnings.length ? (
            <ul className="warn-list">{preview.warnings.map((w) => <li key={w}>{w}</li>)}</ul>
          ) : null}

          <h3>
            <InfoTip text="Current drift across portfolio assets per the latest Simulate. Assets with skippedDueToThreshold=true do not enter the plan.">
              Drift
            </InfoTip>
          </h3>
          <table className="data-table">
            <thead>
              <tr>
                <th>
                  <InfoTip text="Asset ticker from an active target.">Asset</InfoTip>
                </th>
                <th>
                  <InfoTip text="Drift as a weight delta: actual weight − target. Positive — overweight, negative — underweight.">
                    Δ weight
                  </InfoTip>
                </th>
                <th>
                  <InfoTip text="true — |Δ weight| is below the threshold, asset is excluded from the plan. false — asset is in the plan.">
                    Below threshold
                  </InfoTip>
                </th>
              </tr>
            </thead>
            <tbody>
              {preview.drift.map((d) => (
                <tr key={d.assetSymbol}>
                  <td>{d.assetSymbol}</td>
                  <td>{bpsToWeight(d.driftWeightBps)}</td>
                  <td>{String(d.skippedDueToThreshold)}</td>
                </tr>
              ))}
            </tbody>
          </table>

          <h3>
            <InfoTip text="The list of orders the worker would build for the current drift and threshold. Nothing is sent at this stage.">
              Order plan
            </InfoTip>
          </h3>
          {preview.plannedOrders.length === 0 ? (
            <p className="muted">No plan rows for the current threshold.</p>
          ) : (
            <table className="data-table">
              <thead>
                <tr>
                  <th>
                    <InfoTip text="Venue market identifier (BASE-QUOTE, e.g. ETH-USDC) and the base asset ticker in the portfolio.">
                      Market / asset
                    </InfoTip>
                  </th>
                  <th>
                    <InfoTip text="buy — buy the base asset on this market; sell — sell.">Side</InfoTip>
                  </th>
                  <th>
                    <InfoTip text="Quantity in units of the base asset.">Qty</InfoTip>
                  </th>
                  <th>
                    <InfoTip text="Limit price in the quote currency. For type=market it is an estimated price used for notional.">
                      Price
                    </InfoTip>
                  </th>
                </tr>
              </thead>
              <tbody>
                {preview.plannedOrders.map((o, i) => (
                  <tr key={`${o.market}:${i}:${o.price}`}>
                    <td>
                      <div>{o.market}</div>
                      <div className="muted">{o.assetSymbol}</div>
                    </td>
                    <td>{o.side}</td>
                    <td>{o.qty}</td>
                    <td>{o.price}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      ) : (
        <p className="muted">Hint: run Simulate first to see risks and live-blocking.</p>
      )}

      <DriftImitator portfolioId={portfolioId} />
    </div>
  );
}
