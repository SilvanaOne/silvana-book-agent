import type { VenueName } from "./types.js";

/** Строка для GET `/api/venues/status` по адаптерам из этого пакета (без Silvana — её собирает apps/api по env RPC/JWT). */
export type ManagedAdapterStatusRow = Readonly<{
  venue: VenueName;
  configured: boolean;
  mode: "stub" | "simulated_live" | "live" | "scaffold_rest" | "not_configured";
  notes: readonly string[];
}>;

function hasOkxTradingKeys(env: Readonly<NodeJS.ProcessEnv>): boolean {
  const a = env.OKX_API_KEY?.trim();
  const s = env.OKX_API_SECRET?.trim();
  const p = env.OKX_API_PASSPHRASE?.trim();
  return !!(a?.length && s?.length && p?.length);
}

function okxSimulatedTrading(env: Readonly<NodeJS.ProcessEnv>): boolean {
  const m = env.OKX_TRADING_MODE?.trim().toLowerCase();
  return m === "paper" || m === "demo" || m === "simulated" || m === "1" || m === "true" || m === "yes";
}

function trimTruth(raw: string | undefined): boolean {
  const s = raw?.trim().toLowerCase();
  return s === "1" || s === "true" || s === "yes";
}

/** Снимок конфигурации remote-адаптеров (должен отражать логику `resolve*VenueAdapterFromEnv`). */
export function managedAdapterStatusRows(env: NodeJS.ProcessEnv = process.env): ManagedAdapterStatusRow[] {
  const okxConfigured = hasOkxTradingKeys(env);

  const okx: ManagedAdapterStatusRow = {
    venue: "okx",
    configured: okxConfigured,
    mode: okxConfigured ? (okxSimulatedTrading(env) ? "simulated_live" : "live") : "not_configured",
    notes: okxConfigured ? [] : ["Set OKX_API_KEY, OKX_API_SECRET, OKX_API_PASSPHRASE"],
  };

  const templeStub = trimTruth(env.TEMPLE_STUB_ACCEPT);
  const templeBase = env.TEMPLE_API_BASE_URL?.trim();
  const temple: ManagedAdapterStatusRow = {
    venue: "temple",
    configured: !!(templeStub || templeBase?.length),
    mode: templeStub ? "stub" : templeBase?.length ? "scaffold_rest" : "not_configured",
    notes:
      templeStub || templeBase?.length
        ? []
        : ["Set TEMPLE_API_BASE_URL or TEMPLE_STUB_ACCEPT=1 (see Stage 6)"],
  };

  const liqStub = trimTruth(env.OKX_LIQUID_STUB_ACCEPT);
  const liqBase = env.OKX_LIQUID_API_BASE_URL?.trim();
  const liquid: ManagedAdapterStatusRow = {
    venue: "okx-liquid",
    configured: !!(liqStub || liqBase?.length),
    mode: liqStub ? "stub" : liqBase?.length ? "scaffold_rest" : "not_configured",
    notes: !liqStub && !liqBase?.length ? ["Set OKX_LIQUID_STUB_ACCEPT or OKX_LIQUID_API_BASE_URL once RFQ REST is wired"] : [],
  };

  const bncStub = trimTruth(env.BINANCE_OTC_STUB_ACCEPT);
  const bncLive =
    env.BINANCE_OTC_ENABLED?.trim() === "1" &&
    !!(env.BINANCE_OTC_API_KEY?.trim() && env.BINANCE_OTC_API_SECRET?.trim());
  const otc: ManagedAdapterStatusRow = {
    venue: "binance-otc",
    configured: !!(bncStub || bncLive),
    mode: bncStub ? "stub" : bncLive ? "scaffold_rest" : "not_configured",
    notes: bncLive
      ? ["BINANCE_OTC_ENABLED is on and keys are loaded; live REST OTC is gated behind a compliance review"]
      : bncStub
        ? []
        : ["OTC API and keys are allowed only after compliance review"],
  };

  return [okx, temple, liquid, otc];
}
