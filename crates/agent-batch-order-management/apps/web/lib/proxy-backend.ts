import { NextResponse } from "next/server";

import { backendAuthHeaders, backendBaseUrl } from "./backend";

/** Проброс статуса и тела без повторного JSON.parse/stringify там, где возможно. */
export async function forwardBackend(pathname: string, init?: RequestInit): Promise<NextResponse> {
  const url = `${backendBaseUrl()}${pathname.startsWith("/") ? pathname : `/${pathname}`}`;

  const res = await fetch(url, {
    ...init,

    headers: {
      Accept: "application/json",
      ...backendAuthHeaders(),
      ...(init?.headers ?? {}),
    },
    cache: "no-store",
  });

  const bodyText = await res.text();

  return new NextResponse(bodyText, {

    status: res.status,

    headers: {

      "content-type": res.headers.get("content-type") ?? "application/json; charset=utf-8",

    },
  });
}
