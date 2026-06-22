import { Router } from "express";
import type { VenueName } from "@batch-order/venue-adapters";
import { managedAdapterStatusRows } from "@batch-order/venue-adapters";

const VENUE_LABELS: Record<VenueName, string> = {
  silvana: "Silvana Orderbook",
  okx: "OKX Spot (REST v5)",
  temple: "Temple / Canton",
  "okx-liquid": "OKX Liquidity (RFQ)",
  "binance-otc": "Binance OTC",
};

/** Порядок карточек в UI оператора. */
const STATUS_ORDER: readonly VenueName[] = ["silvana", "okx", "temple", "okx-liquid", "binance-otc"];

function executionRouterEnabledFromEnv(env: Readonly<NodeJS.ProcessEnv>): boolean {
  const v = env.EXECUTION_ROUTER_ENABLED?.trim().toLowerCase();
  return v === "1" || v === "true" || v === "yes";
}

function silvanaStatusRow(env: Readonly<NodeJS.ProcessEnv>): {
  venue: VenueName;
  label: string;
  configured: boolean;
  mode: string;
  notes: string[];
} {
  const rpc = env.SILVANA_RPC?.trim();
  const jwt = env.SILVANA_JWT?.trim();
  const ok = !!(rpc?.length && jwt?.length);

  const notes: string[] = [];
  if (!rpc?.length) notes.push("SILVANA_RPC is not set");
  if (!jwt?.length) notes.push("SILVANA_JWT is not set (worker may run in plan_only without submit)");

  return {
    venue: "silvana",
    label: VENUE_LABELS.silvana,

    configured: ok,
    mode: ok ? "rpc_ready" : "incomplete_credentials",
    notes: ok ? [] : notes,
  };
}

export const venuesRouter = Router();

/** GET `/api/venues/status` — снимок конфигурации секретов; без торговых вызовов. */

venuesRouter.get("/status", (_req, res) => {
  const checkedAt = new Date().toISOString();
  const executionRouterEnabled = executionRouterEnabledFromEnv(process.env);

  const remoteRows = managedAdapterStatusRows(process.env);
  const byVenue = new Map(remoteRows.map((r) => [r.venue, r]));

  const venues = STATUS_ORDER.map((id) => {
    if (id === "silvana") {
      return silvanaStatusRow(process.env);

    }

    const r = byVenue.get(id);
    if (!r) {

      return {
        venue: id,
        label: VENUE_LABELS[id],

        configured: false,
        mode: "unknown",
        notes: ["internal: missing diagnostics row"],
      };
    }

    return {
      venue: r.venue,
      label: VENUE_LABELS[r.venue],

      configured: r.configured,
      mode: r.mode,

      notes: [...r.notes],
    };

  });

  res.json({
    checkedAt,
    executionRouterEnabled,
    venues,

  });

});
