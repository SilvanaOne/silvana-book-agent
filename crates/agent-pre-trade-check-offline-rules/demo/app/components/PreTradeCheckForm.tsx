"use client";

import { useState } from "react";
import { DEFAULT_RULES } from "@/lib/pretradecheck-engine";

export type FormValues = {
  rulesJson: string;
  orderArrivalPerTick: string;
  startingPrice: string;
};

const DEFAULT_RULES_JSON = JSON.stringify(DEFAULT_RULES, null, 2);

const DEFAULTS: FormValues = {
  rulesJson: DEFAULT_RULES_JSON,
  orderArrivalPerTick: "0.5",
  startingPrice: "0.15",
};

type Props = Readonly<{ disabled: boolean; onStart: (v: FormValues) => Promise<void> | void }>;

export function PreTradeCheckForm({ disabled, onStart }: Props) {
  const [v, setV] = useState<FormValues>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function upd<K extends keyof FormValues>(k: K, val: FormValues[K]) { setV((p) => ({ ...p, [k]: val })); }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setErr(null);
    setBusy(true);
    try { await onStart(v); } catch (ex) { setErr((ex as Error).message); } finally { setBusy(false); }
  }

  return (
    <form onSubmit={submit} className="stack">
      <div>
        <label>Rules (JSON)</label>
        <textarea
          value={v.rulesJson}
          onChange={(e) => upd("rulesJson", e.target.value)}
          disabled={disabled || busy}
          spellCheck={false}
          style={{ minHeight: 180, fontFamily: "ui-monospace, monospace", fontSize: 12 }}
        />
      </div>
      <div className="grid-2">
        <div>
          <label>Order arrival / tick (Poisson λ)</label>
          <input type="number" step="any" min="0" max="10"
            value={v.orderArrivalPerTick}
            onChange={(e) => upd("orderArrivalPerTick", e.target.value)}
            disabled={disabled || busy} />
        </div>
        <div>
          <label>Starting price (mid seed)</label>
          <input type="number" step="any"
            value={v.startingPrice}
            onChange={(e) => upd("startingPrice", e.target.value)}
            disabled={disabled || busy} />
        </div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={disabled || busy}>{busy ? "Starting…" : "Start Pre-Trade Check"}</button>
    </form>
  );
}

// ---------------------------------------------------------------------------
// Manual check block: sends one order through the currently active rules.
// ---------------------------------------------------------------------------

type ManualResult = {
  decision: "accept" | "reject";
  failedRules: string[];
  notional: number;
};

export function ManualCheck() {
  const [market, setMarket] = useState("CC-USDC");
  const [side, setSide] = useState<"buy" | "sell">("buy");
  const [quantity, setQuantity] = useState("10");
  const [price, setPrice] = useState("1.02");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [result, setResult] = useState<ManualResult | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setErr(null);
    setResult(null);
    setBusy(true);
    try {
      const r = await fetch("/api/pre-trade-check-offline-rules/check-one", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ market, side, quantity: Number(quantity), price: Number(price) }),
      });
      const j = (await r.json()) as Record<string, unknown>;
      if (!r.ok) throw new Error((j.error as string | undefined) ?? `HTTP ${r.status}`);
      setResult({
        decision: j.decision as ManualResult["decision"],
        failedRules: (j.failedRules as string[]) ?? [],
        notional: (j.notional as number) ?? 0,
      });
    } catch (ex) {
      setErr((ex as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={submit} className="stack">
      <div className="grid-2">
        <div><label>Market</label><input value={market} onChange={(e) => setMarket(e.target.value)} disabled={busy} /></div>
        <div>
          <label>Side</label>
          <select value={side} onChange={(e) => setSide(e.target.value as "buy" | "sell")} disabled={busy}>
            <option value="buy">buy</option>
            <option value="sell">sell</option>
          </select>
        </div>
      </div>
      <div className="grid-2">
        <div><label>Quantity</label><input type="number" step="any" value={quantity} onChange={(e) => setQuantity(e.target.value)} disabled={busy} /></div>
        <div><label>Price</label><input type="number" step="any" value={price} onChange={(e) => setPrice(e.target.value)} disabled={busy} /></div>
      </div>
      {err && <div className="negative mono" style={{ fontSize: 13 }}>{err}</div>}
      <button type="submit" className="primary" disabled={busy}>{busy ? "Checking…" : "Check now"}</button>
      {result && (
        <div className="mono" style={{ fontSize: 13, padding: 10, background: "var(--bg-card)", borderRadius: 6 }}>
          <div>
            decision:{" "}
            <span className={result.decision === "accept" ? "positive" : "negative"}>
              {result.decision.toUpperCase()}
            </span>
          </div>
          <div>notional: {result.notional.toFixed(4)}</div>
          {result.failedRules.length > 0 ? (
            <div>failed rules: {result.failedRules.join(", ")}</div>
          ) : (
            <div className="muted">no rules failed</div>
          )}
        </div>
      )}
    </form>
  );
}
