"use client";

import type { SignatureState, SignOperation } from "@/lib/signature-engine";
import { SIGNATURE_ALGO } from "@/lib/signature-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function truncate(s: string | null | undefined, n: number): string {
  if (!s) return "—";
  return s.length > n ? s.slice(0, n) + "…" : s;
}

type Props = Readonly<{ signature: SignatureState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ signature, walk }: Props) {
  const c = signature?.config;
  const stateLabel = signature === null ? "idle" : signature.status === "monitoring" ? "armed" : "stopped";
  const lastOp: SignOperation | undefined = signature?.operations.at(-1);
  const lastVerify: SignOperation | undefined = [...(signature?.operations ?? [])]
    .reverse()
    .find((o) => o.kind === "verify");
  void walk;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head">
          <div className="info-card-title">RUNTIME</div>
        </div>
        <div className="kv-row">
          <span className="k">strategy mode</span>
          <span className="v">signature</span>
        </div>
        <div className="kv-row">
          <span className="k">silvana host</span>
          <span className="v">standalone-dev</span>
        </div>
        <div className="kv-row">
          <span className="k">poll interval</span>
          <span className="v">1000 ms</span>
        </div>
        <div className="kv-row">
          <span className="k">persist</span>
          <span className="v">off</span>
        </div>
      </div>

      <div className="info-card">
        <div className="info-card-head">
          <div className="info-card-title">SIG CONFIG</div>
        </div>
        {c ? (
          <>
            <div className="kv-row">
              <span className="k">auto interval</span>
              <span className="v">{c.autoDemoIntervalSecs}s</span>
            </div>
            <div className="kv-row">
              <span className="k">tamper rate</span>
              <span className="v accent">{(c.tamperRate * 100).toFixed(1)}%</span>
            </div>
            <div className="kv-row">
              <span className="k">starting price</span>
              <span className="v">{fmt(c.startingPrice)}</span>
            </div>
            <div className="kv-row">
              <span className="k">algorithm</span>
              <span className="v">{SIGNATURE_ALGO}</span>
            </div>
          </>
        ) : (
          <>
            <div className="kv-row">
              <span className="k">auto interval</span>
              <span className="v faint">—</span>
            </div>
            <div className="kv-row">
              <span className="k">tamper rate</span>
              <span className="v faint">—</span>
            </div>
            <div className="kv-row">
              <span className="k">starting price</span>
              <span className="v faint">—</span>
            </div>
            <div className="kv-row">
              <span className="k">algorithm</span>
              <span className="v faint">{SIGNATURE_ALGO}</span>
            </div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head">
          <div className="info-card-title">KEY</div>
        </div>
        <div className="kv-row">
          <span className="k">public (b64url)</span>
          <span className="v mono">{truncate(signature?.publicKey, 16)}</span>
        </div>
        <div className="kv-row">
          <span className="k">private (masked)</span>
          <span className="v mono">{signature?.privateKeyMasked ?? "—"}</span>
        </div>
        <div className="kv-row">
          <span className="k">algorithm</span>
          <span className="v">{SIGNATURE_ALGO}</span>
        </div>
        <div className="kv-row">
          <span className="k">rotations</span>
          <span className="v">{signature?.stats.keyGenerated ?? 0}</span>
        </div>
      </div>

      <div className="info-card">
        <div className="info-card-head">
          <div className="info-card-title">STATS</div>
        </div>
        <div className="kv-row">
          <span className="k">signed</span>
          <span className="v">{signature?.stats.signed ?? 0}</span>
        </div>
        <div className="kv-row">
          <span className="k">verified</span>
          <span className="v">{signature?.stats.verified ?? 0}</span>
        </div>
        <div className="kv-row">
          <span className="k">passed</span>
          <span className="v positive">{signature?.stats.verifiedOk ?? 0}</span>
        </div>
        <div className="kv-row">
          <span className="k">failed</span>
          <span className="v negative">{signature?.stats.verifiedFailed ?? 0}</span>
        </div>
      </div>

      <div className="info-card">
        <div className="info-card-head">
          <div className="info-card-title">LAST OP</div>
        </div>
        <div className="kv-row">
          <span className="k">kind</span>
          <span className="v">{lastOp?.kind ?? "—"}</span>
        </div>
        <div className="kv-row">
          <span className="k">msg</span>
          <span className="v mono">{truncate(lastOp?.input, 22)}</span>
        </div>
        <div className="kv-row">
          <span className="k">signature</span>
          <span className="v mono">{truncate(lastOp?.output, 20)}</span>
        </div>
        <div className="kv-row">
          <span className="k">last verify</span>
          <span className={`v ${lastVerify ? (lastVerify.verified ? "positive" : "negative") : ""}`}>
            {lastVerify ? (lastVerify.verified ? "OK" : "FAIL") : "—"}
          </span>
        </div>
      </div>

      <div className="info-card">
        <div className="info-card-head">
          <div className="info-card-title">STATE</div>
        </div>
        <div className="kv-row">
          <span className="k">status</span>
          <span className="v">{stateLabel}</span>
        </div>
        <div className="kv-row">
          <span className="k">operations</span>
          <span className="v">{signature?.operations.length ?? 0}</span>
        </div>
        <div className="kv-row">
          <span className="k">current price</span>
          <span className="v">{fmt(signature?.currentPrice)}</span>
        </div>
        <div className="kv-row">
          <span className="k">seq</span>
          <span className="v">{signature?.seq ?? 0}</span>
        </div>
      </div>
    </div>
  );
}
