import type { OrderIntent, OrderResult, VenueAdapter } from "../types.js";
import { toOkxSpotInstId } from "./inst.js";
import type { OkxEnvelope, OkxTradeOrderRow } from "./rest.js";
import { OkxRestClient } from "./rest.js";

export type OkxVenueAdapterDeps = Readonly<{
  apiKey: string;
  secretKey: string;
  passphrase: string;
  baseUrl?: string;
  simulated?: boolean;
}>;

const OKX_MAX_CLORDID_LEN = 32;

function truncateClOrdId(ref: string | undefined): string | undefined {
  const t = ref?.trim();
  if (!t || !t.length) return undefined;
  return t.length <= OKX_MAX_CLORDID_LEN ? t : t.slice(0, OKX_MAX_CLORDID_LEN);
}

function venueAdapterOkxStubSubmitError(): VenueAdapter {
  return {
    venue: "okx",
    async submitOrder(_intent: OrderIntent): Promise<OrderResult> {
      throw new Error(
        "@batch-order/venue-adapters: OKX REST не сконфигурирован — задайте OKX_API_KEY, OKX_API_SECRET, OKX_API_PASSPHRASE или используйте createOkxVenueAdapter(deps)",
      );
    },
    async cancelOrder(_params: Readonly<{ orderId: string; market?: string }>): Promise<void> {
      /**/
    },
    async replaceOrder(): Promise<OrderResult> {
      throw new Error("@batch-order/venue-adapters: OKX replaceOrder не реализован (используйте cancel + submit)");
    },
  };
}

/** REST OKX v5 spot `tdMode=cash` (Этап 5b плана). */
export function createOkxVenueAdapter(deps: OkxVenueAdapterDeps): VenueAdapter {
  const client = new OkxRestClient(deps);

  return {
    venue: "okx",
    async submitOrder(intent: OrderIntent): Promise<OrderResult> {
      if (intent.type === "market") {
        throw new Error("okx: spot market orders not supported in MVP (use limit or extend adapter with tgtCcy)");
      }

      const px = intent.price?.trim();
      if (!px?.length) {
        throw new Error("okx: limit order requires intent.price");
      }

      const instId = toOkxSpotInstId(intent.market);

      const body: Record<string, string> = {
        instId,
        tdMode: "cash",
        side: intent.side,
        ordType: "limit",
        sz: intent.qty.trim(),
        px,
      };

      const cl = truncateClOrdId(intent.clientOrderRef);
      if (cl !== undefined) body.clOrdId = cl;

      const resp: OkxEnvelope<OkxTradeOrderRow[]> = await client.placeSpotOrder(body);
      const row = Array.isArray(resp.data) && resp.data.length > 0 ? resp.data[0] : undefined;
      if (row === undefined) {
        throw new Error("okx: unexpected empty data[] in trade response");
      }

      const sc = row.sCode ?? "unknown";
      if (sc !== "0") {
        throw new Error(`okx trade order rejected sCode=${sc} msg=${row.sMsg ?? ""}`);
      }

      const ordId = row.ordId?.trim();
      if (!ordId?.length) {
        throw new Error("okx: response missing ordId");
      }

      return {
        venue: "okx",
        orderId: ordId,
        status: "accepted",
        raw: resp,
      };
    },

    async cancelOrder(paramsOrder: Readonly<{ orderId: string; market?: string }>): Promise<void> {
      const m = paramsOrder.market?.trim();
      if (!m?.length) {
        throw new Error("okx: cancelOrder requires params.market as instId (e.g. BTC-USDT)");
      }
      await client.cancelOrder({ instId: toOkxSpotInstId(m), ordId: paramsOrder.orderId.trim() });
    },

    async replaceOrder(_orderId: string, _next: OrderIntent): Promise<OrderResult> {
      throw new Error("@batch-order/venue-adapters: OKX replaceOrder — cancel + submit или amend-order позже");
    },
  };
}

function okxTradingSimulatedFromEnv(env: NodeJS.ProcessEnv): boolean {
  const m = env.OKX_TRADING_MODE?.trim().toLowerCase();
  return (
    m === "paper" ||
    m === "demo" ||
    m === "simulated" ||
    m === "1" ||
    m === "true" ||
    m === "yes"
  );
}

/**
 * Если задан полный ключ, возвращает адаптер с REST; иначе — заглушку с понятным сообщением при submit.
 */
export function resolveOkxVenueAdapterFromEnv(env: NodeJS.ProcessEnv = process.env): VenueAdapter {
  const apiKey = env.OKX_API_KEY?.trim();
  const secretKey = env.OKX_API_SECRET?.trim();
  const passphrase = env.OKX_API_PASSPHRASE?.trim();

  if (!apiKey?.length || !secretKey?.length || !passphrase?.length) {
    return venueAdapterOkxStubSubmitError();
  }

  const baseUrl = env.OKX_API_BASE_URL?.trim();

  return createOkxVenueAdapter({
    apiKey,
    secretKey,
    passphrase,
    ...(baseUrl && baseUrl.length > 0 ? { baseUrl } : {}),
    simulated: okxTradingSimulatedFromEnv(env),
  });
}

/** Явная заглушка (как прежнее поведение createOkxVenueAdapter без ключей из этапов 1–4). */
export function createOkxVenueAdapterStub(): VenueAdapter {
  return venueAdapterOkxStubSubmitError();
}
