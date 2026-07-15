"use client";

import { type FormEvent, useState } from "react";

import { InfoTip } from "@/app/components/InfoTip";

type PreviewRisk =
  | { ok: true }
  | { ok: false; code: string; message: string };

type PreviewOutcome = Readonly<{
  checkedAt?: string;
  intent?: unknown;
  routerConfig?: unknown;
  chosenVenue?: string;
  risk?: PreviewRisk;
  error?: string;
}>;

/** Dry-run form for `/api/execution/preview-route` (Stage 9). */
export function PreviewRouteCard() {
  const [market, setMarket] = useState("ETH-USDC");
  const [side, setSide] = useState<"buy" | "sell">("buy");
  const [type, setType] = useState<"limit" | "market">("limit");
  const [qty, setQty] = useState("1");
  const [price, setPrice] = useState("2000");
  const [venue, setVenue] = useState("");
  const [execProfile, setExecProfile] = useState("");
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<PreviewOutcome | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setLoading(true);
    try {
      const body: Record<string, string> = { market, side, type, qty };
      if (type === "limit" && price.trim().length > 0) body.price = price.trim();
      const v = venue.trim();
      if (v.length > 0) body.venue = v;
      const ep = execProfile.trim();
      if (ep.length > 0) body.execProfile = ep;

      const res = await fetch("/api/backend/execution/preview-route", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });

      const text = await res.text();

      try {
        const data = JSON.parse(text) as Record<string, unknown>;
        if (!res.ok) {
          const err =
            typeof data.error === "string" && data.error.length > 0 ? data.error : text;
          setResult({ error: err });
        } else {
          setResult(data as PreviewOutcome);
        }
      } catch {
        setResult({ error: text || String(res.status) });
      }
    } finally {
      setLoading(false);
    }
  }

  return (
    <section className="silv-panel">
      <h2 style={{ marginTop: 0 }}>
        <InfoTip text="Dry-run /api/execution/preview-route. Takes an order intent and the router config and returns the chosen venue plus a risk gates summary, without sending any order.">
          Route preview
        </InfoTip>
      </h2>

      <p className="muted" style={{ marginTop: "-0.35rem" }}>
        Dry-run <code>/api/execution/preview-route</code>, no order is sent.
      </p>

      <form onSubmit={submit} className="stack" style={{ maxWidth: 520 }}>
        <label>
          <InfoTip text="Venue market identifier in BASE-QUOTE format (for example, ETH-USDC, BTC-USDT).">
            Market
          </InfoTip>
          <input value={market} onChange={(e) => setMarket(e.target.value)} required />
        </label>

        <label>
          <InfoTip text="buy — buy the base asset; sell — sell.">Side</InfoTip>
          <select value={side} onChange={(e) => setSide(e.target.value as "buy" | "sell")}>
            <option value="buy">buy</option>
            <option value="sell">sell</option>
          </select>
        </label>

        <label>
          <InfoTip text="limit — limit order (Price required). market — market order, executed at the best ask/bid.">
            Type
          </InfoTip>
          <select value={type} onChange={(e) => setType(e.target.value as "limit" | "market")}>
            <option value="limit">limit</option>
            <option value="market">market</option>
          </select>
        </label>

        <label>
          <InfoTip text="Quantity in units of the base asset (a number).">Qty</InfoTip>
          <input value={qty} onChange={(e) => setQty(e.target.value)} required />
        </label>

        {type === "limit" ? (
          <label>
            <InfoTip text="Limit price in the quote currency. Applies only to type=limit.">
              Price
            </InfoTip>
            <input value={price} onChange={(e) => setPrice(e.target.value)} />
          </label>
        ) : null}

        <label>
          <InfoTip text="Force a venue for the calculation (silvana / okx / …). Empty — let the router choose based on execProfile and configuration.">
            Venue (forced, optional)
          </InfoTip>
          <input placeholder="silvana | okx | …" value={venue} onChange={(e) => setVenue(e.target.value)} />
        </label>

        <label>
          <InfoTip text="Hint to the router about the execution type: book (CLOB), rfq (request-for-quote), block (block trade), otc.">
            execProfile (optional)
          </InfoTip>
          <input placeholder="book | rfq | block | otc" value={execProfile} onChange={(e) => setExecProfile(e.target.value)} />
        </label>

        <div>
          <button type="submit" disabled={loading} className="silv-btn silv-btn--primary">
            {loading ? "…" : "Preview"}
          </button>
        </div>
      </form>

      {result ? (
        <pre className="json-block" style={{ marginTop: "1rem" }}>
          {JSON.stringify(result, null, 2)}
        </pre>
      ) : null}
    </section>
  );
}
