"use client";

import { useRouter } from "next/navigation";
import { useMemo, useState } from "react";

import { InfoTip } from "@/app/components/InfoTip";
import { RebalanceNowModal } from "@/app/portfolio/rebalance-now-modal";

const SUM_TOLERANCE = 0.001;

type Row = {
  assetSymbol: string;
  weight: string;
  minWeight: string;
  maxWeight: string;
  enabled: boolean;
};

type Props = Readonly<{
  portfolioId: string;
  initial: ReadonlyArray<{
    assetSymbol: string;
    targetWeight: string;
    minWeight: string | null;
    maxWeight: string | null;
    enabled: boolean;
  }>;
}>;

function rowsFromInitial(initial: Props["initial"]): Row[] {
  return initial.map((t) => ({
    assetSymbol: t.assetSymbol,
    weight: t.targetWeight,
    minWeight: t.minWeight ?? "",
    maxWeight: t.maxWeight ?? "",
    enabled: t.enabled,
  }));
}

export function TargetsEditor(props: Props) {
  const router = useRouter();
  const [mode, setMode] = useState<"view" | "edit">("view");
  const [rows, setRows] = useState<Row[]>(() => rowsFromInitial(props.initial));
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [rebalanceOpen, setRebalanceOpen] = useState(false);

  const sumEnabled = useMemo(
    () =>
      rows.reduce((acc, r) => {
        if (!r.enabled) return acc;
        const w = Number(r.weight);
        return Number.isFinite(w) ? acc + w : acc;
      }, 0),
    [rows],
  );
  const sumOk = Math.abs(sumEnabled - 1) <= SUM_TOLERANCE;

  function startEdit() {
    setRows(rowsFromInitial(props.initial));
    setErr(null);
    setMode("edit");
  }

  function cancel() {
    setRows(rowsFromInitial(props.initial));
    setErr(null);
    setMode("view");
  }

  function updateRow(idx: number, patch: Partial<Row>) {
    setRows((prev) => prev.map((r, i) => (i === idx ? { ...r, ...patch } : r)));
  }

  function addRow() {
    setRows((prev) => [...prev, { assetSymbol: "", weight: "0", minWeight: "", maxWeight: "", enabled: true }]);
  }

  function removeRow(idx: number) {
    setRows((prev) => prev.filter((_, i) => i !== idx));
  }

  async function save() {
    setErr(null);
    for (const r of rows) {
      if (r.assetSymbol.trim() === "") {
        setErr("Each row must have an asset symbol.");
        return;
      }
      const w = Number(r.weight);
      if (!Number.isFinite(w) || w < 0 || w > 1) {
        setErr(`Weight for ${r.assetSymbol || "(empty)"} must be a number in [0, 1].`);
        return;
      }
    }
    if (!sumOk) {
      setErr(`Sum of enabled weights must equal 1.0 ± ${SUM_TOLERANCE} (got ${sumEnabled.toFixed(6)}).`);
      return;
    }

    setBusy(true);
    try {
      const res = await fetch(`/api/backend/portfolio/${encodeURIComponent(props.portfolioId)}/targets`, {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          targets: rows.map((r) => ({
            assetSymbol: r.assetSymbol.trim(),
            weight: Number(r.weight),
            enabled: r.enabled,
            minWeight: r.minWeight.trim() === "" ? null : Number(r.minWeight),
            maxWeight: r.maxWeight.trim() === "" ? null : Number(r.maxWeight),
          })),
        }),
      });
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
      setMode("view");
      router.refresh();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section>
      <h2>
        <InfoTip text="Active portfolio allocation targets. The sum of weight where enabled=true should be ≈ 1.0 (100%).">
          Targets
        </InfoTip>
        {mode === "view" ? (
          <>
            {" "}
            <button type="button" className="silv-link-btn" onClick={startEdit}>
              Edit
            </button>
          </>
        ) : null}
      </h2>

      {mode === "view" ? (
        <div className="stack">
          <table className="data-table">
            <thead>
              <tr>
                <th>
                  <InfoTip text="Asset ticker (for example, BTC, ETH, USDT). The quote asset is configured separately — quoteCurrency field.">
                    Asset
                  </InfoTip>
                </th>
                <th>
                  <InfoTip text="Target weight of the asset in the portfolio, a fraction from 0 to 1. For example, 0.25 = 25% of the portfolio value.">
                    Weight
                  </InfoTip>
                </th>
                <th>
                  <InfoTip text="Minimum allowed asset weight. If the actual weight drops below it, rebalance will buy more. A dash (—) means no lower bound is set.">
                    Min
                  </InfoTip>
                </th>
                <th>
                  <InfoTip text="Maximum allowed asset weight. If exceeded, rebalance will sell the surplus. A dash (—) means no upper bound is set.">
                    Max
                  </InfoTip>
                </th>
                <th>
                  <InfoTip text="Target is enabled and included in the rebalance calculation. Disabled targets are ignored by the portfolio-engine.">
                    On
                  </InfoTip>
                </th>
              </tr>
            </thead>
            <tbody>
              {props.initial.map((t) => (
                <tr key={t.assetSymbol}>
                  <td>{t.assetSymbol}</td>
                  <td>{t.targetWeight}</td>
                  <td>{t.minWeight ?? "—"}</td>
                  <td>{t.maxWeight ?? "—"}</td>
                  <td>{String(t.enabled)}</td>
                </tr>
              ))}
            </tbody>
          </table>

          <div className="btn-row">
            <button type="button" onClick={() => setRebalanceOpen(true)}>
              Rebalance
            </button>
          </div>

          {rebalanceOpen ? (
            <RebalanceNowModal
              portfolioId={props.portfolioId}
              onClose={() => setRebalanceOpen(false)}
            />
          ) : null}
        </div>
      ) : (
        <div className="stack">
          <table className="data-table">
            <thead>
              <tr>
                <th>Asset</th>
                <th>Weight</th>
                <th>Min</th>
                <th>Max</th>
                <th>On</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={i}>
                  <td>
                    <input
                      value={r.assetSymbol}
                      onChange={(e) => updateRow(i, { assetSymbol: e.target.value })}
                      spellCheck={false}
                      style={{ width: "8em" }}
                    />
                  </td>
                  <td>
                    <input
                      type="number"
                      step="0.0001"
                      min={0}
                      max={1}
                      value={r.weight}
                      onChange={(e) => updateRow(i, { weight: e.target.value })}
                      style={{ width: "7em" }}
                    />
                  </td>
                  <td>
                    <input
                      value={r.minWeight}
                      onChange={(e) => updateRow(i, { minWeight: e.target.value })}
                      placeholder="—"
                      style={{ width: "6em" }}
                    />
                  </td>
                  <td>
                    <input
                      value={r.maxWeight}
                      onChange={(e) => updateRow(i, { maxWeight: e.target.value })}
                      placeholder="—"
                      style={{ width: "6em" }}
                    />
                  </td>
                  <td>
                    <input
                      type="checkbox"
                      checked={r.enabled}
                      onChange={(e) => updateRow(i, { enabled: e.target.checked })}
                    />
                  </td>
                  <td>
                    <button type="button" onClick={() => removeRow(i)} aria-label="Remove row">
                      ✕
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          <p className={sumOk ? "muted" : "err"}>
            Sum of enabled weights: <strong>{sumEnabled.toFixed(4)}</strong>{" "}
            {sumOk ? "✓" : `(must be ≈ 1.0 ± ${SUM_TOLERANCE})`}
          </p>

          {err ? <p className="err">{err}</p> : null}

          <div className="btn-row">
            <button type="button" onClick={addRow} disabled={busy}>
              + Add asset
            </button>
            <button type="button" onClick={save} disabled={busy || !sumOk}>
              {busy ? "Saving…" : "Save"}
            </button>
            <button type="button" onClick={cancel} disabled={busy}>
              Cancel
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
