/**
 * DEMO build — no backend, no network.
 *
 * In the real web app this module issued HTTP requests to `apps/api`. Here it resolves
 * everything in-process against synthetic fixtures (see `lib/demo-backend.ts`). The public
 * signature is unchanged so the server components that call `backendJson` are untouched.
 */
import { handleBackend } from "./demo-backend";

/** Kept for signature compatibility; the demo never issues network requests. */
export function backendBaseUrl(): string {
  return "demo://in-process";
}

/** Kept for signature compatibility; the demo has no auth. */
export function backendAuthHeaders(): HeadersInit {
  return {};
}

/** In-process JSON resolver for server components and actions (was `fetch` to apps/api). */
export async function backendJson<T>(
  pathname: string,
  init?: RequestInit,
): Promise<{ ok: true; data: T } | { ok: false; status: number; body: string }> {
  const method = (init?.method ?? "GET").toUpperCase();
  const bodyText = typeof init?.body === "string" ? init.body : undefined;

  const res = handleBackend(method, pathname.startsWith("/") ? pathname : `/${pathname}`, bodyText);

  if (res.status < 200 || res.status >= 300) {
    return { ok: false, status: res.status, body: JSON.stringify(res.json) };
  }
  return { ok: true, data: res.json as T };
}
