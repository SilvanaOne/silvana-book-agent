export const COOKIE = "sv.sid";
export const SESSION_TTL_SECS = 12 * 60 * 60;

export function setSessionCookieHeader(token: string): string {
  return `${COOKIE}=${token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${SESSION_TTL_SECS}`;
}
