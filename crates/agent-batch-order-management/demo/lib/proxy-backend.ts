import { NextResponse } from "next/server";

import { handleBackend } from "./demo-backend";

/**
 * DEMO build — no backend, no network.
 *
 * The `app/api/backend/**` route handlers call this to reach `apps/api`. Here it resolves
 * the request in-process against synthetic fixtures and returns a `NextResponse`, so no
 * request ever leaves the process. Signature is unchanged; route handlers are untouched.
 */
export async function forwardBackend(pathname: string, init?: RequestInit): Promise<NextResponse> {
  const method = (init?.method ?? "GET").toUpperCase();
  const bodyText = typeof init?.body === "string" ? init.body : undefined;

  const res = handleBackend(method, pathname.startsWith("/") ? pathname : `/${pathname}`, bodyText);

  return new NextResponse(JSON.stringify(res.json), {
    status: res.status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}
