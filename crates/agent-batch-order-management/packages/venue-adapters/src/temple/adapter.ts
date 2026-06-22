import type { OrderIntent, OrderResult, VenueAdapter } from "../types.js";
import { TempleHttpClient } from "./client.js";
import { toTempleInstrumentId } from "./mapping.js";

/** Пути submit и long-polling стакана — см. официальный OpenAPI Temple. Опционально: `VenueAdapter.subscribeOrderEvents`. */
export type TempleVenueAdapterDeps = Readonly<{
  /** Принять заявку без сети (локальная отладка; не использовать в проде). */
  stubAccept?: boolean;
  http?: TempleHttpClient;
  /**
   * POST относительно `TempleHttpClient` base URL.
   * Пример: `/v1/trading/orders`; по умолчанию `/orders`.
   */
  orderSubmitRelPath?: string;
}>;

const DEFAULT_ORDER_REL = "/orders";

function truthyTempleStub(raw: string | undefined): boolean {
  const s = raw?.trim().toLowerCase();
  return s === "1" || s === "true" || s === "yes";
}

function parseTempleOrderId(raw: Record<string, unknown>): string | undefined {
  const keys = ["order_id", "orderId", "id", "external_id", "externalId"] as const;
  for (const k of keys) {
    const v = raw[k];
    if (typeof v === "string" && v.trim().length > 0) return v.trim();
  }
  return undefined;
}

function venueAdapterTempleStubSubmitError(): VenueAdapter {
  return {
    venue: "temple",
    async submitOrder(_intent: OrderIntent): Promise<OrderResult> {
      throw new Error(
        "@batch-order/venue-adapters: Temple не сконфигурирован — задайте TEMPLE_API_BASE_URL (+ TEMPLE_AUTH_BEARER | TEMPLE_API_KEY) или TEMPLE_STUB_ACCEPT=1 для офлайна",
      );
    },
    async cancelOrder(_params: Readonly<{ orderId: string; market?: string }>): Promise<void> {
      /**/
    },
    async replaceOrder(): Promise<OrderResult> {
      throw new Error("@batch-order/venue-adapters: Temple replaceOrder не реализован");
    },
  };
}

async function templeSubmitViaHttp(intent: OrderIntent, http: TempleHttpClient, rel: string): Promise<OrderResult> {
  if (intent.type === "market") {
    throw new Error(
      "temple: MVP — только spot limit (расширьте адаптер для market после спецификации Temple)",
    );
  }
  const px = intent.price?.trim();
  if (!px?.length) {
    throw new Error("temple: limit без price недопустим");
  }

  const body: Record<string, unknown> = {
    instrumentId: toTempleInstrumentId(intent.market),
    side: intent.side,
    type: intent.type,
    qty: intent.qty.trim(),
    px,
  };
  const cl = intent.clientOrderRef?.trim();
  if (cl?.length) body.clientOrderRef = cl;

  const { status, json } = await http.postJson<Record<string, unknown>>(rel, body);

  if (status < 200 || status >= 300) {
    throw new Error(`temple REST submit failed HTTP ${status} body=${JSON.stringify(json)}`);
  }

  const id = parseTempleOrderId(json);
  if (!id?.length) {
    throw new Error(`temple REST: ответ без order id: ${JSON.stringify(json)}`);
  }

  return { venue: "temple", orderId: id, status: "accepted", raw: json };
}

export function createTempleVenueAdapter(deps: TempleVenueAdapterDeps = {}): VenueAdapter {
  const rel = deps.orderSubmitRelPath?.trim().length ? deps.orderSubmitRelPath.trim() : DEFAULT_ORDER_REL;

  if (deps.stubAccept === true) {
    return {
      venue: "temple",
      async submitOrder(intent: OrderIntent): Promise<OrderResult> {
        if (intent.type === "market") {
          throw new Error("temple stub: MVP — только limit ордера");
        }
        const px = intent.price?.trim();
        if (!px?.length) {
          throw new Error("temple stub: limit без price не поддерживается");
        }
        const ref =
          intent.clientOrderRef?.trim().length ?? 0 ? intent.clientOrderRef!.trim().slice(0, 48) : `t_${Date.now()}`;
        const instId = toTempleInstrumentId(intent.market);
        return {
          venue: "temple",
          orderId: `temple-stub:${instId}:${ref}`,
          status: "accepted",
          raw: { stubAccept: true, instrumentId: instId, intent },
        };
      },
      async cancelOrder(_params: Readonly<{ orderId: string; market?: string }>): Promise<void> {
        /**/
      },
      async replaceOrder(): Promise<OrderResult> {
        throw new Error("@batch-order/venue-adapters: Temple replaceOrder не реализован");
      },
    };
  }

  const http = deps.http;
  if (!http) {
    return venueAdapterTempleStubSubmitError();
  }

  return {
    venue: "temple",

    submitOrder(intent: OrderIntent): Promise<OrderResult> {
      return templeSubmitViaHttp(intent, http, rel);
    },

    async cancelOrder(params: Readonly<{ orderId: string; market?: string }>): Promise<void> {
      void params;
      throw new Error("@batch-order/venue-adapters: Temple cancelOrder не проводён против REST до согласования API");
    },

    async replaceOrder(): Promise<OrderResult> {
      throw new Error("@batch-order/venue-adapters: Temple replaceOrder не реализован");
    },
  };
}

/**
 * Если `TEMPLE_STUB_ACCEPT` — офлайн-режим иначе `TEMPLE_API_BASE_URL` + опционально Bearer / API-ключ.
 */
export function resolveTempleVenueAdapterFromEnv(env: NodeJS.ProcessEnv = process.env): VenueAdapter {
  if (truthyTempleStub(env.TEMPLE_STUB_ACCEPT)) {
    return createTempleVenueAdapter({ stubAccept: true });
  }

  const baseUrl = env.TEMPLE_API_BASE_URL?.trim();
  if (!baseUrl?.length) {
    return venueAdapterTempleStubSubmitError();
  }

  const bearer = env.TEMPLE_AUTH_BEARER?.trim();
  const apiKey = env.TEMPLE_API_KEY?.trim();
  const apiKeyHeaderName = env.TEMPLE_API_KEY_HEADER?.trim();

  const client = new TempleHttpClient({
    baseUrl,
    ...(bearer && bearer.length > 0 ? { bearerToken: bearer } : {}),
    ...(apiKey && apiKey.length > 0 ? { apiKey, ...(apiKeyHeaderName?.length ? { apiKeyHeaderName } : {}) } : {}),
  });

  const orderSubmitRelPath = env.TEMPLE_ORDER_SUBMIT_PATH?.trim();

  return createTempleVenueAdapter({
    http: client,
    ...(orderSubmitRelPath?.length ? { orderSubmitRelPath } : {}),
  });
}

export function createTempleVenueAdapterStub(): VenueAdapter {
  return venueAdapterTempleStubSubmitError();
}
