"use client";

import { useRouter } from "next/navigation";
import { useEffect, useState } from "react";

import { InfoTip } from "@/app/components/InfoTip";
import { withBasePath } from "@/lib/base-path";

type Transfer = Readonly<{
  from: string;
  to: string;
  amountFrom: string;
  amountTo: string;
  txHash: string;
  note: string;
}>;

type ApiResult = Readonly<{
  portfolioId: string;
  mode: "plan_only" | "rpc";
  isDemo: boolean;
  nav: string;
  transfers: ReadonlyArray<Transfer>;
  plannedOrdersCount: number;
}>;

type Stage = "running" | "done" | "error";

const SCAN_URL_TEMPLATE = (
  process.env.NEXT_PUBLIC_SILVANA_SCAN_URL ?? "https://silvanascan.io/tx/{hash}"
).trim();

function txExplorerUrl(hash: string): string {
  if (SCAN_URL_TEMPLATE.includes("{hash}")) {
    return SCAN_URL_TEMPLATE.replace("{hash}", hash);
  }
  return `${SCAN_URL_TEMPLATE.replace(/\/$/, "")}/${hash}`;
}

function shortHash(hash: string): string {
  if (hash.length <= 14) return hash;
  return `${hash.slice(0, 8)}…${hash.slice(-6)}`;
}

function PaperclipLink(props: Readonly<{ tx: Transfer; isDemo: boolean }>) {
  const { tx, isDemo } = props;
  const tooltip = isDemo
    ? `${tx.note} · demo (plan_only) — no on-chain settlement; hash ${tx.txHash} is generated locally for UI testing`
    : `${tx.note} · open transaction ${tx.txHash} in the Silvana block explorer (new tab)`;

  return (
    <InfoTip text={tooltip} label="Open in block explorer">
      <a
        href={txExplorerUrl(tx.txHash)}
        target="_blank"
        rel="noreferrer noopener"
        aria-label="Open in block explorer"
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: "0.25em",
          textDecoration: "none",
        }}
      >
        <span aria-hidden="true" style={{ fontSize: "1.1em" }}>
          📎
        </span>
        <code style={{ fontSize: "0.85em" }}>{shortHash(tx.txHash)}</code>
      </a>
    </InfoTip>
  );
}

export function RebalanceNowModal(props: Readonly<{ portfolioId: string; onClose: () => void }>) {
  const router = useRouter();
  const [stage, setStage] = useState<Stage>("running");
  const [result, setResult] = useState<ApiResult | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    async function run() {
      try {
        const res = await fetch(
          withBasePath(`/api/backend/portfolio/${encodeURIComponent(props.portfolioId)}/rebalance-now`),
          {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: "{}",
          },
        );
        const text = await res.text();
        if (cancelled) return;
        if (!res.ok) {
          let msg = text;
          try {
            const j = JSON.parse(text) as { message?: string; error?: string };
            msg = j.message ?? j.error ?? text;
          } catch {}
          setErr(`HTTP ${res.status}: ${msg}`);
          setStage("error");
          return;
        }
        const parsed = JSON.parse(text) as ApiResult;
        setResult(parsed);
        setStage("done");
      } catch (e) {
        if (cancelled) return;
        setErr(e instanceof Error ? e.message : String(e));
        setStage("error");
      }
    }
    void run();
    return () => {
      cancelled = true;
    };
  }, [props.portfolioId]);

  function close() {
    router.refresh();
    props.onClose();
  }

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="rebalance-now-title"
      onClick={(e) => {
        if (e.target === e.currentTarget && stage !== "running") close();
      }}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(15, 18, 30, 0.55)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1000,
      }}
    >
      <div
        style={{
          background: "var(--silv-bg, #fff)",
          color: "var(--silv-text, #111)",
          borderRadius: "12px",
          padding: "1.5rem 1.75rem",
          minWidth: "min(620px, 92vw)",
          maxWidth: "min(720px, 96vw)",
          maxHeight: "90vh",
          overflowY: "auto",
          boxShadow: "0 24px 60px rgba(0,0,0,0.35)",
        }}
      >
        <h2 id="rebalance-now-title" style={{ marginTop: 0 }}>
          Rebalance
        </h2>

        {stage === "running" ? (
          <div className="stack">
            <p>
              <span
                aria-hidden="true"
                style={{
                  display: "inline-block",
                  width: "1em",
                  height: "1em",
                  border: "2px solid currentColor",
                  borderRightColor: "transparent",
                  borderRadius: "50%",
                  animation: "spin 1s linear infinite",
                  marginRight: "0.5em",
                  verticalAlign: "-0.15em",
                }}
              />
              Rebalancing portfolio to current targets…
            </p>
            <p className="muted">Building plan, computing transfers, settling positions.</p>
            <style>{`@keyframes spin { from { transform: rotate(0deg);} to { transform: rotate(360deg);} }`}</style>
          </div>
        ) : null}

        {stage === "error" ? (
          <div className="stack">
            <p className="err">Rebalance failed: {err}</p>
            <div className="btn-row" style={{ marginTop: "1rem" }}>
              <button type="button" onClick={close}>
                Close
              </button>
            </div>
          </div>
        ) : null}

        {stage === "done" && result ? (
          <div className="stack">
            {result.isDemo ? (
              <p className="muted" style={{ fontSize: "0.9em" }}>
                <strong>Demo mode</strong> (worker is in <code>plan_only</code>, no SILVANA_JWT). Snapshots were
                rewritten to target weights to close the loop visually; transaction hashes below are generated
                locally for UI testing and do not exist on-chain.
              </p>
            ) : null}

            {result.transfers.length === 0 ? (
              <p>
                Portfolio is already at targets — nothing to rebalance. Drift was already below threshold or the
                plan was empty. Portfolio value: <code>{result.nav}</code>.
              </p>
            ) : (
              <>
                <p>
                  Done. {result.transfers.length} transfer{result.transfers.length === 1 ? "" : "s"} executed
                  to bring the portfolio back to its targets. Portfolio value: <code>{result.nav}</code>.
                </p>
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>From</th>
                      <th>To</th>
                      <th>Amount sent</th>
                      <th>Amount received</th>
                      <th>
                        <InfoTip text="Transaction hash on the Silvana block explorer. Click the paperclip to open in a new tab. In demo (plan_only) mode the hash is locally generated and not on-chain.">
                          Tx
                        </InfoTip>
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {result.transfers.map((t) => (
                      <tr key={t.txHash}>
                        <td>{t.from}</td>
                        <td>{t.to}</td>
                        <td>{t.amountFrom}</td>
                        <td>{t.amountTo}</td>
                        <td>
                          <PaperclipLink tx={t} isDemo={result.isDemo} />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </>
            )}

            <div className="btn-row" style={{ marginTop: "1.5rem" }}>
              <button type="button" onClick={close}>
                OK
              </button>
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}
