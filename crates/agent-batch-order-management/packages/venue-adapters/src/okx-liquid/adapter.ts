import type { OrderIntent, OrderResult, VenueAdapter } from "../types.js";
import { OkxLiquidHttpClient } from "./rest.js";

export type OkxLiquidVenueAdapterDeps = Readonly<{
  /** Локальный «RFQ создан» без сети (`OKX_LIQUID_STUB_ACCEPT`). */
  stubAccept?: boolean;
  /** После проводки Liquidity Marketplace — дергать `submitRfqRequest` вместо throw. */
  http?: OkxLiquidHttpClient;
}>;

function truthyLiquidStub(raw: string | undefined): boolean {
  const s = raw?.trim().toLowerCase();
  return s === "1" || s === "true" || s === "yes";
}

function venueAdapterOkxLiquidStubSubmitError(): VenueAdapter {
  return {
    venue: "okx-liquid",
    async submitOrder(_intent: OrderIntent): Promise<OrderResult> {
      throw new Error(
        "@batch-order/venue-adapters: OKX Liquidity Marketplace не включён — задайте OKX_LIQUID_STUB_ACCEPT=1 для офлайна или подключите RFQ REST (см. okx-liquid/rest.ts)",
      );
    },
    async cancelOrder(_params: Readonly<{ orderId: string; market?: string }>): Promise<void> {
      /**/
    },
    async replaceOrder(): Promise<OrderResult> {
      throw new Error("@batch-order/venue-adapters: OKX Liquid replaceOrder — state RFQ позже");
    },
  };
}

export function createOkxLiquidVenueAdapter(deps: OkxLiquidVenueAdapterDeps = {}): VenueAdapter {
  if (deps.stubAccept === true) {
    return {
      venue: "okx-liquid",
      async submitOrder(intent: OrderIntent): Promise<OrderResult> {
        const ref =
          intent.clientOrderRef?.trim().length ?? 0
            ? intent.clientOrderRef!.trim().slice(0, 48)
            : `rfq_${Date.now()}`;
        const payload = deps.http?.rfqPayloadFromIntent(intent, ref);
        return {
          venue: "okx-liquid",
          orderId: `okx-liq-rfq-stub:${ref}`,
          status: "accepted",
          raw: {
            stubAccept: true,
            note: "RFQ lifecycle quoted→accepted→settled — см. Этап 7 плана; не эквивалентен немедленному fill",
            payload,
          },
        };
      },
      async cancelOrder(_params: Readonly<{ orderId: string; market?: string }>): Promise<void> {
        /**/
      },
      async replaceOrder(): Promise<OrderResult> {
        throw new Error("@batch-order/venue-adapters: OKX Liquid replaceOrder — state RFQ позже");
      },
    };
  }

  const http = deps.http;
  if (!http) {
    return venueAdapterOkxLiquidStubSubmitError();
  }

  return {
    venue: "okx-liquid",
    async submitOrder(intent: OrderIntent): Promise<OrderResult> {
      void http.rfqPayloadFromIntent(intent);
      /** Плейсхолдер до готовности реального REST; сохраните тело через `rfqPayloadFromIntent` при интеграции. */
      await http.submitRfqRequest();
      throw new Error("okx-liquid: submitRfqRequest не должен возвращаться — проверьте okx-liquid/rest.ts");
    },
    async cancelOrder(params: Readonly<{ orderId: string; market?: string }>): Promise<void> {
      void params;
      throw new Error("@batch-order/venue-adapters: OKX Liquid cancelOrder — см. состояние RFQ");
    },
    async replaceOrder(): Promise<OrderResult> {
      throw new Error("@batch-order/venue-adapters: OKX Liquid replaceOrder — см. состояние RFQ");
    },
  };
}

export function resolveOkxLiquidVenueAdapterFromEnv(env: NodeJS.ProcessEnv = process.env): VenueAdapter {
  if (truthyLiquidStub(env.OKX_LIQUID_STUB_ACCEPT)) {
    return createOkxLiquidVenueAdapter({ stubAccept: true });
  }

  const baseUrl = env.OKX_LIQUID_API_BASE_URL?.trim();
  if (baseUrl?.length) {
    const apiKey = env.OKX_LIQUID_API_KEY?.trim();
    const apiSecret = env.OKX_LIQUID_API_SECRET?.trim();
    const passphrase = env.OKX_LIQUID_API_PASSPHRASE?.trim();
    return createOkxLiquidVenueAdapter({
      http: new OkxLiquidHttpClient({
        baseUrl,
        ...(apiKey?.length ? { apiKey } : {}),
        ...(apiSecret?.length ? { apiSecret } : {}),
        ...(passphrase?.length ? { passphrase } : {}),
      }),
    });
  }

  return venueAdapterOkxLiquidStubSubmitError();
}

export function createOkxLiquidVenueAdapterStub(): VenueAdapter {
  return venueAdapterOkxLiquidStubSubmitError();
}
