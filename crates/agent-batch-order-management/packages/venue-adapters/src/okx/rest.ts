import { createHmac } from "node:crypto";

const DEFAULT_BASE_URL = "https://www.okx.com";

export type OkxHttpConfig = Readonly<{
  apiKey: string;
  secretKey: string;
  passphrase: string;
  baseUrl?: string;
  /** Demo / paper: заголовок `x-simulated-trading: 1` (ключи из демо-аккаунта OKX). */
  simulated?: boolean;
}>;

/** Ответ обёртки REST v5 (поле `code` строкой). */
export type OkxEnvelope<T> = Readonly<{
  code: string;
  msg: string;
  data: T;
}>;

export type OkxTradeOrderRow = Readonly<{
  ordId?: string;
  clOrdId?: string;
  sCode?: string;
  sMsg?: string;
}>;

/** prehash: timestamp + METHOD + requestPath + body */
export function okxSignBas64(params: Readonly<{ secretKey: string; timestamp: string; method: string; requestPath: string; body: string }>): string {
  const pre = params.timestamp + params.method.toUpperCase() + params.requestPath + params.body;
  return createHmac("sha256", params.secretKey).update(pre).digest("base64");
}

export class OkxRestClient {
  constructor(private readonly cfg: OkxHttpConfig) {}

  private headers(method: string, requestPath: string, body: string): Record<string, string> {
    const ts = new Date().toISOString();
    const sign = okxSignBas64({
      secretKey: this.cfg.secretKey,
      timestamp: ts,
      method,
      requestPath,
      body,
    });
    const h: Record<string, string> = {
      "OK-ACCESS-KEY": this.cfg.apiKey,
      "OK-ACCESS-SIGN": sign,
      "OK-ACCESS-TIMESTAMP": ts,
      "OK-ACCESS-PASSPHRASE": this.cfg.passphrase,
    };
    if (this.cfg.simulated) h["x-simulated-trading"] = "1";
    return h;
  }

  async request<T>(method: "GET" | "POST", requestPath: string, bodyObj?: unknown): Promise<T> {
    const body = bodyObj === undefined ? "" : JSON.stringify(bodyObj);
    const base = this.cfg.baseUrl?.trim() || DEFAULT_BASE_URL;
    const url = `${base.replace(/\/$/, "")}${requestPath}`;

    const res = await fetch(url, {
      method,
      headers: {
        ...this.headers(method, requestPath, body),
        ...(method === "POST" ? { "Content-Type": "application/json" } : {}),
      },
      body: method === "POST" ? body : undefined,
    });

    const text = await res.text();
    let json: unknown;
    try {
      json = JSON.parse(text);
    } catch {
      throw new Error(`okx: non-JSON HTTP ${res.status}: ${text.slice(0, 500)}`);
    }

    const wrap = json as OkxEnvelope<unknown>;
    if (wrap.code !== "0") {
      throw new Error(`okx api error ${wrap.code}: ${wrap.msg ?? text}`);
    }
    return json as T;
  }

  async placeSpotOrder(body: Readonly<Record<string, string>>): Promise<OkxEnvelope<OkxTradeOrderRow[]>> {
    return this.request<OkxEnvelope<OkxTradeOrderRow[]>>("POST", "/api/v5/trade/order", body);
  }

  async cancelOrder(body: Readonly<{ instId: string; ordId: string }>): Promise<OkxEnvelope<unknown>> {
    return this.request<OkxEnvelope<unknown>>("POST", "/api/v5/trade/cancel-order", body);
  }
}
