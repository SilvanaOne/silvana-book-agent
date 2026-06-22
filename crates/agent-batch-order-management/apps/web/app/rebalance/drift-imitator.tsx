"use client";

import Link from "next/link";
import { useState } from "react";

import { InfoTip } from "@/app/components/InfoTip";

type ApplyResult = Readonly<{
  portfolioId: string;
  nav: string;
  applied: {
    assetSymbol: string;
    driftWeight: number;
    newActualWeights: ReadonlyArray<{ assetSymbol: string; weight: string }>;
  };
}>;

function isDemoToolsEnabled(): boolean {
  const v = (process.env.NEXT_PUBLIC_DEMO_TOOLS ?? "").trim().toLowerCase();
  return v === "1" || v === "true" || v === "yes";
}

export function DriftImitator(props: Readonly<{ portfolioId: string }>) {
  const [assetSymbol, setAssetSymbol] = useState("CC");
  const [driftWeight, setDriftWeight] = useState("0.15");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [result, setResult] = useState<ApplyResult | null>(null);

  if (!isDemoToolsEnabled()) {
    return null;
  }

  async function apply() {
    setErr(null);
    setResult(null);
    const dw = Number(driftWeight);
    if (!Number.isFinite(dw)) {
      setErr("Drift must be a number (decimal share, e.g. 0.15 or -0.10).");
      return;
    }
    if (assetSymbol.trim() === "") {
      setErr("Asset symbol is required.");
      return;
    }
    setBusy(true);
    try {
      const res = await fetch(
        `/api/backend/portfolio/${encodeURIComponent(props.portfolioId)}/imitate-drift`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ assetSymbol: assetSymbol.trim(), driftWeight: dw }),
        },
      );
      const text = await res.text();
      if (!res.ok) {
        let msg = text;
        try {
          const j = JSON.parse(text) as { message?: string; error?: string };
          msg = j.message ?? j.error ?? text;
        } catch {}
        setErr(`HTTP ${res.status}: ${msg}`);
        return;
      }
      const parsed = JSON.parse(text) as ApplyResult;
      setResult(parsed);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="silv-panel" style={{ marginTop: "2rem", borderStyle: "dashed" }}>
      <h2>
        Manual Drift Imitator{" "}
        <span className="muted" style={{ fontSize: "0.8em" }}>
          (demo / testing only)
        </span>
      </h2>
      <p className="muted">
        This is a testing tool for the dashboard — it pretends the market moved, so you can see how the
        agent would react. You pick one of your two assets and a shift amount (for example, <code>0.10</code>{" "}
        means &quot;by 10% of the portfolio&quot;). That asset&apos;s share of the portfolio goes up (or down,
        if you enter a negative number) by that much, and the other asset moves the opposite way to keep the
        whole thing balanced.
      </p>
      <p className="muted">
        The total value of the portfolio and the asset prices stay exactly the same — only the proportions
        change. After you click <strong>Apply</strong>, open the Portfolio page: the donut chart and the bars
        will reflect the new mix, and Simulate will show the orders the agent would build to bring everything
        back to the targets you saved. Nothing is sent to the exchange.
      </p>
      <p className="muted">
        Works for portfolios with two or more enabled targets. When you shift one asset, the
        remaining ones absorb the change proportionally to their own target weights — so the
        ratios between counter-assets stay the same.
      </p>

      <div className="form-grid">
        <label>
          <InfoTip text="Which asset to push off-target. Must be one of the enabled targets on this portfolio.">
            Asset
          </InfoTip>
          <input value={assetSymbol} onChange={(e) => setAssetSymbol(e.target.value)} spellCheck={false} />
        </label>
        <label>
          <InfoTip text="Drift as a weight delta (decimal share). +0.15 = asset becomes overweight by 15% of portfolio value. −0.10 = underweight by 10%. Math: new_actual_weight = target + this value.">
            Drift (weight delta)
          </InfoTip>
          <input
            value={driftWeight}
            onChange={(e) => setDriftWeight(e.target.value)}
            placeholder="example: 0.15 or -0.10"
          />
        </label>
      </div>

      <div className="btn-row" style={{ marginTop: "1.5rem" }}>
        <button type="button" onClick={() => void apply()} disabled={busy}>
          {busy ? "Applying…" : "Apply drift"}
        </button>
      </div>

      {err ? <p className="err">{err}</p> : null}

      {result ? (
        <div className="stack">
          <p className="muted">
            Applied. NAV preserved at <code>{result.nav}</code>. New actual weights:
          </p>
          <ul>
            {result.applied.newActualWeights.map((w) => (
              <li key={w.assetSymbol}>
                <code>{w.assetSymbol}</code> · <strong>{Number(w.weight).toFixed(4)}</strong>
              </li>
            ))}
          </ul>
          <p>
            Check the result on the <Link href={`/portfolio?id=${encodeURIComponent(props.portfolioId)}`}>Portfolio page</Link>.
          </p>
        </div>
      ) : null}
    </section>
  );
}
