import type { NextRequest } from "next/server";

export const COOKIE = "sv.sid";
export const SESSION_TTL_SECS = 12 * 60 * 60;

export function readCookie(req: NextRequest, name: string): string | undefined {
  return req.cookies.get(name)?.value;
}

export function setSessionCookieHeader(token: string): string {
  return `${COOKIE}=${token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${SESSION_TTL_SECS}`;
}

export function clearSessionCookieHeader(): string {
  return `${COOKIE}=; Path=/; HttpOnly; Max-Age=0`;
}
