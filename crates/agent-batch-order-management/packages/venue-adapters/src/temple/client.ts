/**
 * Прототип HTTP-клиента для Temple / Canton institutional API (Этап 6).
 * Конкретные пути и схема JSON — см. официальный Temple CLI / OpenAPI перед продом.
 */

export type TempleHttpClientDeps = Readonly<{
  baseUrl: string;
  bearerToken?: string;
  apiKey?: string;
  /** Заголовок для API-ключа, если Bearer не используется. */
  apiKeyHeaderName?: string;
  fetchImpl?: typeof fetch;
}>;

function trimTrailingSlash(u: string): string {
  return u.replace(/\/+$/, "");
}

export class TempleHttpClient {
  private readonly deps: Omit<TempleHttpClientDeps, "fetchImpl"> & { fetchImpl: typeof fetch };

  constructor(deps: TempleHttpClientDeps) {
    this.deps = { ...deps, fetchImpl: deps.fetchImpl ?? fetch };
  }

  get baseUrlResolved(): string {
    return trimTrailingSlash(this.deps.baseUrl.trim());
  }

  private headers(): Headers {
    const h = new Headers();
    h.set("Content-Type", "application/json");
    const bearer = this.deps.bearerToken?.trim();
    if (bearer?.length) {
      h.set("Authorization", `Bearer ${bearer}`);
    }
    const key = this.deps.apiKey?.trim();
    if (key?.length) {
      const name = this.deps.apiKeyHeaderName?.trim() ?? "X-Api-Key";
      h.set(name, key);
    }
    return h;
  }

  async postJson<T = unknown>(
    relativePathOrUrl: string,
    body: unknown,
    init?: Readonly<{ signal?: AbortSignal }>,
  ): Promise<{ status: number; json: T }> {
    const path = relativePathOrUrl.startsWith("http") ? relativePathOrUrl : `${this.baseUrlResolved}${relativePathOrUrl.startsWith("/") ? "" : "/"}${relativePathOrUrl}`;
    const res = await this.deps.fetchImpl(path, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify(body),
      signal: init?.signal,
    });
    let json = undefined as unknown;
    try {
      json = await res.json();
    } catch {
      /**/
    }
    return { status: res.status, json: json as T };
  }

  /** Минимальная проверка доступности перед submit (polling / readiness). */
  async getJson<T = unknown>(
    relativePathOrUrl: string,
    init?: Readonly<{ signal?: AbortSignal }>,
  ): Promise<{ status: number; json: T }> {
    const path = relativePathOrUrl.startsWith("http") ? relativePathOrUrl : `${this.baseUrlResolved}${relativePathOrUrl.startsWith("/") ? "" : "/"}${relativePathOrUrl}`;
    const res = await this.deps.fetchImpl(path, {
      method: "GET",
      headers: this.headers(),
      signal: init?.signal,
    });
    let json = undefined as unknown;
    try {
      json = await res.json();
    } catch {
      /**/
    }
    return { status: res.status, json: json as T };
  }
}
