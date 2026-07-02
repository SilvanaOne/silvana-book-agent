"use client";

import { useState } from "react";

export type FormValues = {
  autoDemoIntervalSecs: string;
  tamperRate: string;
  startingPrice: string;
};

const DEFAULTS: FormValues = {
  autoDemoIntervalSecs: "5",
  tamperRate: "0.1",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function SignatureForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) {
    setV((p) => ({ ...p, [k]: val }));
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setErr(null);
    setBusy(true);
    try {
      await onStart(v);
    } catch (ex) {
      setErr((ex as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div>
          <label>Auto-demo interval (secs)</label>
          <input
            type="number"
            step="any"
            value={v.autoDemoIntervalSecs}
            onChange={(e) => upd("autoDemoIntervalSecs", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
        <div>
          <label>Tamper rate (0..1)</label>
          <input
            type="number"
            step="any"
            value={v.tamperRate}
            onChange={(e) => upd("tamperRate", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
      </div>
      <div className="grid-2">
        <div>
          <label>Starting price (mid seed)</label>
          <input
            type="number"
            step="any"
            value={v.startingPrice}
            onChange={(e) => upd("startingPrice", e.target.value)}
            disabled={disabled || busy}
          />
        </div>
        <div style={{ alignSelf: "end" }}>
          <div className="muted mono" style={{ fontSize: 12 }}>
            Every N secs → sign + verify roundtrip. Tamper rate flips one byte of the sig before verify.
          </div>
        </div>
      </div>
      {err && (
        <div className="negative mono" style={{ fontSize: 13 }}>
          {err}
        </div>
      )}
      <button type="submit" className="primary" disabled={disabled || busy}>
        {busy ? "Starting…" : "Start Signature"}
      </button>
    </form>
  );
}

/* ---------------------------------------------------------------------
 * Manual sign / verify / rotate panel — mounted separately in page.tsx.
 * ------------------------------------------------------------------- */

type ManualProps = Readonly<{
  disabled: boolean;
  onSign: (message: string) => Promise<{ signature: string; publicKey: string } | { error: string }>;
  onVerify: (message: string, signature: string) => Promise<{ ok: boolean } | { error: string }>;
  onRotate: () => Promise<{ publicKey: string } | { error: string }>;
}>;

type Result =
  | { kind: "sign"; signature: string; publicKey: string }
  | { kind: "verify"; ok: boolean }
  | { kind: "rotate"; publicKey: string }
  | { kind: "error"; message: string };

export function ManualSignPanel({ disabled, onSign, onVerify, onRotate }: ManualProps) {
  const [message, setMessage] = useState("hello silvana");
  const [signature, setSignature] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<Result | null>(null);

  async function doSign() {
    if (!message) return;
    setBusy(true);
    try {
      const r = await onSign(message);
      if ("error" in r) setResult({ kind: "error", message: r.error });
      else {
        setResult({ kind: "sign", signature: r.signature, publicKey: r.publicKey });
        setSignature(r.signature);
      }
    } catch (ex) {
      setResult({ kind: "error", message: (ex as Error).message });
    } finally {
      setBusy(false);
    }
  }

  async function doVerify() {
    if (!message || !signature) return;
    setBusy(true);
    try {
      const r = await onVerify(message, signature);
      if ("error" in r) setResult({ kind: "error", message: r.error });
      else setResult({ kind: "verify", ok: r.ok });
    } catch (ex) {
      setResult({ kind: "error", message: (ex as Error).message });
    } finally {
      setBusy(false);
    }
  }

  async function doRotate() {
    setBusy(true);
    try {
      const r = await onRotate();
      if ("error" in r) setResult({ kind: "error", message: r.error });
      else setResult({ kind: "rotate", publicKey: r.publicKey });
    } catch (ex) {
      setResult({ kind: "error", message: (ex as Error).message });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="stack">
      <div>
        <label>Message to sign</label>
        <textarea
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          disabled={disabled || busy}
          rows={2}
          style={{ width: "100%", resize: "vertical", fontFamily: "ui-monospace, monospace", fontSize: 13 }}
        />
      </div>
      <div>
        <label>Signature (base64)</label>
        <input
          value={signature}
          onChange={(e) => setSignature(e.target.value)}
          disabled={disabled || busy}
          placeholder="paste or auto-fill from Sign"
          style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12 }}
        />
      </div>
      <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
        <button type="button" className="primary" onClick={doSign} disabled={disabled || busy || !message}>
          Sign now
        </button>
        <button
          type="button"
          className="ghost"
          onClick={doVerify}
          disabled={disabled || busy || !message || !signature}
        >
          Verify
        </button>
        <button type="button" className="ghost" onClick={doRotate} disabled={disabled || busy}>
          Rotate key
        </button>
      </div>

      {result && (
        <div
          className="mono"
          style={{
            fontSize: 12,
            padding: 8,
            borderRadius: 4,
            background: "var(--bg-card)",
            border: "1px solid #22222c",
            wordBreak: "break-all",
          }}
        >
          {result.kind === "sign" && (
            <>
              <div>
                <span className="muted">sig:</span> {result.signature}
              </div>
              <div>
                <span className="muted">pub:</span> {result.publicKey}
              </div>
            </>
          )}
          {result.kind === "verify" && (
            <div className={result.ok ? "positive" : "negative"}>{result.ok ? "verified OK" : "verify FAIL"}</div>
          )}
          {result.kind === "rotate" && (
            <div>
              <span className="muted">new pub:</span> {result.publicKey}
            </div>
          )}
          {result.kind === "error" && <div className="negative">error: {result.message}</div>}
        </div>
      )}
    </div>
  );
}
