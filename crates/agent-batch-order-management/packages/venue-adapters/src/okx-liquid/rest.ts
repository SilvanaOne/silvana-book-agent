/**
 * Каркас REST для OKX Liquidity Marketplace (RFQ / block, Этап 7).
 * Реальный URL и контракт тела запроса — после выдачи официальной спецификации под ключом.
 */

import type { OrderIntent } from "../types.js";

export type OkxLiquidHttpClientDeps = Readonly<{
  baseUrl: string;
  apiKey?: string;
  apiSecret?: string;
  passphrase?: string;
  fetchImpl?: typeof fetch;
}>;

function trimTrailingSlash(u: string): string {
  return u.replace(/\/+$/, "");
}

export class OkxLiquidHttpClient {
  private readonly deps: Omit<OkxLiquidHttpClientDeps, "fetchImpl"> & { fetchImpl: typeof fetch };

  constructor(deps: OkxLiquidHttpClientDeps) {
    this.deps = { ...deps, fetchImpl: deps.fetchImpl ?? fetch };
  }

  get baseUrlResolved(): string {
    return trimTrailingSlash(this.deps.baseUrl.trim());
  }

  /**
   * Собирает набросок тела RFQ из `OrderIntent` (расширяется под multi-leg позже).
   */
  rfqPayloadFromIntent(intent: OrderIntent, externalRef?: string): Record<string, unknown> {
    return {
      market: intent.market.trim(),
      side: intent.side,
      type: intent.type,
      qty: intent.qty.trim(),
      price: intent.price?.trim(),
      execProfile: intent.execProfile ?? "rfq",
      clientOrderRef: intent.clientOrderRef?.trim(),
      externalRef,
    };
  }

  /**
   * Плейсхолдер POST RFQ — не вызывает сеть без явного включения после согласования API.
   */
  async submitRfqRequest(_relativePath = "/rfq"): Promise<never> {
    throw new Error(
      "okx-liquid: Liquidity Marketplace RFQ REST не подключён — уточните эндпоинты и замените submitRfqRequest в okx-liquid/rest.ts",
    );
  }
}
