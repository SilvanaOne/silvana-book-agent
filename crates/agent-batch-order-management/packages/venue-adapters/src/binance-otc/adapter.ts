import type { OrderIntent, OrderResult, VenueAdapter } from "../types.js";

/** Только OTC / блок-комплайенс; общий Spot retail здесь не подключаем (Этап 8 плана). */
export type BinanceOtcVenueAdapterDeps = Readonly<{
  stubAccept?: boolean;
}>;

function truthyOtcStub(raw: string | undefined): boolean {
  const s = raw?.trim().toLowerCase();
  return s === "1" || s === "true" || s === "yes";
}

function venueBinanceOtcSubmitNotConfigured(): VenueAdapter {
  return {
    venue: "binance-otc",
    async submitOrder(_intent: OrderIntent): Promise<OrderResult> {
      throw new Error(
        "@batch-order/venue-adapters: Binance OTC не включён — отдельные ключи после комплаенса (BINANCE_OTC_*) или BINANCE_OTC_STUB_ACCEPT=1 только для офлайна",
      );
    },
    async cancelOrder(): Promise<void> {
      /**/
    },
    async replaceOrder(): Promise<OrderResult> {
      throw new Error("@batch-order/venue-adapters: Binance OTC replaceOrder не реализован");
    },
  };
}

export function createBinanceOtcVenueAdapter(deps: BinanceOtcVenueAdapterDeps = {}): VenueAdapter {
  if (deps.stubAccept === true) {
    return {
      venue: "binance-otc",
      async submitOrder(intent: OrderIntent): Promise<OrderResult> {
        const ref =
          intent.clientOrderRef?.trim().length ?? 0
            ? intent.clientOrderRef!.trim().slice(0, 40)
            : `otc_${Date.now()}`;
        return {
          venue: "binance-otc",
          orderId: `binance-otc-stub:${ref}`,
          status: "accepted",
          raw: { stubAccept: true, complianceNote: "OTC только по разрешённому процессу" },
        };
      },
      async cancelOrder(): Promise<void> {
        /**/
      },
      async replaceOrder(): Promise<OrderResult> {
        throw new Error("@batch-order/venue-adapters: Binance OTC replaceOrder не реализован");
      },
    };
  }

  return venueBinanceOtcSubmitNotConfigured();
}

export function resolveBinanceOtcVenueAdapterFromEnv(env: NodeJS.ProcessEnv = process.env): VenueAdapter {
  if (truthyOtcStub(env.BINANCE_OTC_STUB_ACCEPT)) {
    return createBinanceOtcVenueAdapter({ stubAccept: true });
  }

  const keyConfigured =
    !!(env.BINANCE_OTC_API_KEY?.trim() && env.BINANCE_OTC_API_SECRET?.trim()) &&
    env.BINANCE_OTC_ENABLED?.trim() === "1";

  /** Ключи + флаг — каркас; отдельный REST OTC без смешения с Binance Spot retail. */
  if (keyConfigured) {
    return {
      venue: "binance-otc",
      async submitOrder(_intent: OrderIntent): Promise<OrderResult> {
        throw new Error("@batch-order/venue-adapters: binance-otc OTC REST не подключён после комплаенса — проводите клиент только с отдельным ревью API");
      },
      async cancelOrder(): Promise<void> {
        /**/
      },
      async replaceOrder(): Promise<OrderResult> {
        throw new Error("@batch-order/venue-adapters: Binance OTC replaceOrder не реализован");
      },
    };
  }

  return venueBinanceOtcSubmitNotConfigured();
}

export function createBinanceOtcVenueAdapterStub(): VenueAdapter {
  return venueBinanceOtcSubmitNotConfigured();
}
