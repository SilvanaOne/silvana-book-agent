/**
 * Прямые запросы к apps/api из Next-сервера и из route handlers прокси.
 * Ключ только server-side (`API_INTERNAL_KEY`), не смешиваем с `NEXT_PUBLIC_*`.
 */
export function backendBaseUrl(): string {
  const u =
    process.env.BACKEND_INTERNAL_URL?.trim() ||
    process.env.NEXT_PUBLIC_API_URL?.trim() ||
    "http://localhost:3000";

  return u.replace(/\/$/, "");
}

export function backendAuthHeaders(): HeadersInit {
  const h: Record<string, string> = {};
  const key = process.env.API_INTERNAL_KEY?.trim();
  if (key && key.length > 0) {
    h.Authorization = `Bearer ${key}`;
  }

  return h;
}

/** JSON от apps/api для server components и actions. */
export async function backendJson<T>(pathname: string, init?: RequestInit): Promise<{ ok: true; data: T } | { ok: false; status: number; body: string }> {
  const url = `${backendBaseUrl()}${pathname.startsWith("/") ? pathname : `/${pathname}`}`;

  const res = await fetch(url, {
    ...init,
    headers: { ...backendAuthHeaders(), ...(init?.headers ?? {}) },
    cache: "no-store",
  });

  const body = await res.text();

  if (!res.ok) {
    return { ok: false, status: res.status, body };
  }

  try {
    return { ok: true, data: JSON.parse(body) as T };
  } catch {
    return { ok: false, status: res.status, body };
  }
}
