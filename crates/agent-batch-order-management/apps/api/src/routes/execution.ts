import { Router } from "express";

import type { OrderIntent, VenueName } from "@batch-order/venue-adapters";

import { chooseVenue, passesRiskRules, routerConfigFromEnv } from "@batch-order/execution-router";

export const executionPreviewRouter = Router();

const KNOWN_VENUES = new Set<VenueName>(["silvana", "okx", "temple", "okx-liquid", "binance-otc"]);

function parseVenueName(v: unknown): VenueName | undefined {
  if (typeof v !== "string") return undefined;
  const s = v.trim() as VenueName;
  return KNOWN_VENUES.has(s) ? s : undefined;

}

function parseExecProfile(v: unknown): OrderIntent["execProfile"] {
  if (typeof v !== "string") return undefined;
  const t = v.trim();
  if (t === "book" || t === "rfq" || t === "block" || t === "otc") return t;
  return undefined;
}

/** POST `/api/execution/preview-route` — dry-run `chooseVenue` без постановки заявки. */


executionPreviewRouter.post("/preview-route", (req, res) => {
  const body = req.body as Record<string, unknown> | undefined;

  if (!body || typeof body !== "object") {
    res.status(400).json({ error: "expected JSON body" });
    return;
  }

  const market = typeof body.market === "string" ? body.market.trim() : "";
  const side = body.side === "sell" ? "sell" : body.side === "buy" ? "buy" : undefined;
  const type = body.type === "market" ? "market" : body.type === "limit" ? "limit" : undefined;
  const qty = typeof body.qty === "string" ? body.qty.trim() : "";

  if (!market?.length || !side || !type || !qty.length) {
    res.status(400).json({
      error: "market, side (buy|sell), type (limit|market), qty are required",
    });
    return;
  }

  const requestedVenue = parseVenueName(body.venue);
  if (body.venue !== undefined && typeof body.venue === "string" && body.venue.trim().length > 0 && requestedVenue === undefined) {

    res.status(400).json({ error: "unknown venue" });
    return;

  }

  const ep = parseExecProfile(body.execProfile);
  if (
    body.execProfile !== undefined &&
    typeof body.execProfile === "string" &&
    body.execProfile.trim().length > 0 &&
    ep === undefined
  ) {
    res.status(400).json({ error: "execProfile must be book|rfq|block|otc when set" });
    return;
  }

  const priceRaw = typeof body.price === "string" ? body.price.trim() : undefined;

  const intent: OrderIntent = {
    market,
    side,
    type,

    qty,
    ...(priceRaw?.length ? { price: priceRaw } : {}),
    ...(ep !== undefined ? { execProfile: ep } : {}),
    ...(requestedVenue !== undefined ? { venue: requestedVenue } : {}),
    ...(typeof body.clientOrderRef === "string" && body.clientOrderRef.trim().length > 0
      ? { clientOrderRef: body.clientOrderRef.trim() }

      : {}),
  };

  const cfg = routerConfigFromEnv(process.env);
  const chosenVenue = chooseVenue(intent, cfg);


  const risk = passesRiskRules(intent, chosenVenue, {});


  res.json({
    checkedAt: new Date().toISOString(),
    intent,

    routerConfig: {
      defaultVenue: cfg.defaultVenue,
      minQtyRfQ: cfg.minQtyRfQ,
      maxQtyOkx: cfg.maxQtyOkx,
      maxQtyLiquid: cfg.maxQtyLiquid ?? null,
    },
    chosenVenue,

    risk,

  });


});
