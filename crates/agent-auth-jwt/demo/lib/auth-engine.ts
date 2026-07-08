// Port of agent-auth-jwt CLI logic to TypeScript. Mirrors
// crates/agent-auth-jwt/src/main.rs: generate / decode / verify / user-id.
//
// The real CLI uses Ed25519 (RFC 8037) — here we produce a mock ES256-style
// header and a deterministic mock base64 signature so a browser can render
// realistic-looking JWTs without dragging in a signing library. The demo is a
// teaching model, not a security tool.

export type JwtHeader = Readonly<{ alg: string; typ: string }>;
export type JwtPayload = Readonly<{
  sub: string;
  role: string;
  iat: number;
  exp: number;
  jti: string;
}>;

export type JwtToken = Readonly<{
  header: JwtHeader;
  payload: JwtPayload;
  signature: string;   // mock base64 fragment
  raw: string;         // full "header.payload.signature" string
  issuedAt: number;    // epoch ms
  expiresAt: number;   // epoch ms
  verified: boolean;   // deterministic pass/fail flag for the demo
}>;

export type AuthConfig = Readonly<{
  role: string;                       // default "trader"
  ttlSecs: number;                    // default 3600
  autoRefreshEnabled: boolean;        // default true
  autoRefreshIntervalSecs: number;    // default 60
  startingPrice: number;              // seed for the synthetic mid feed
}>;

export type AuthStats = {
  generated: number;
  decoded: number;
  verified: number;
  verifiedOk: number;
  expired: number;
};

export type AuthState = {
  config: AuthConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  currentToken: JwtToken | null;
  history: JwtToken[];                // bounded ring of rotated tokens
  lastRotationAt: number | null;
  stats: AuthStats;
};

const MAX_HISTORY = 10;
const SUBJECT = "party-agent-1";

export function initState(config: AuthConfig, startPrice: number, now: number): AuthState {
  const state: AuthState = {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    currentToken: null,
    history: [],
    lastRotationAt: null,
    stats: { generated: 0, decoded: 0, verified: 0, verifiedOk: 0, expired: 0 },
  };
  // Mint an initial token so the UI has something to display immediately.
  state.currentToken = mintToken(state, now);
  state.lastRotationAt = now;
  return state;
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: AuthState, price: number, now: number): { state: AuthState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Expire the live token if we blew past its exp.
  if (state.currentToken && now > state.currentToken.expiresAt) {
    events.push(`TOKEN EXPIRED: jti=${state.currentToken.payload.jti} (exp ${iso(state.currentToken.expiresAt)})`);
    // Push into history as an expired trace.
    pushHistory(state, state.currentToken);
    state.stats.expired += 1;
    state.currentToken = null;
  }

  // Auto-refresh: rotate on the configured cadence, or if we lost the token
  // (e.g. because it just expired above).
  if (state.config.autoRefreshEnabled) {
    const cadenceMs = state.config.autoRefreshIntervalSecs * 1000;
    const dueByCadence = state.lastRotationAt === null || now - state.lastRotationAt >= cadenceMs;
    if (!state.currentToken || dueByCadence) {
      const rotated = state.currentToken;
      const next = mintToken(state, now);
      if (rotated && rotated !== state.currentToken) pushHistory(state, rotated);
      state.currentToken = next;
      state.lastRotationAt = now;
      events.push(`TOKEN ROTATED: jti=${next.payload.jti} exp=${iso(next.expiresAt)} role=${next.payload.role}`);
    }
  }

  return { state, events };
}

/** Manual generation triggered from the UI. Replaces currentToken. */
export function generateToken(
  state: AuthState,
  override: { role?: string; ttlSecs?: number },
  now: number,
): { state: AuthState; token: JwtToken; events: string[] } {
  const role = override.role && override.role.trim().length > 0 ? override.role.trim() : state.config.role;
  const ttlSecs = override.ttlSecs && override.ttlSecs > 0 ? override.ttlSecs : state.config.ttlSecs;
  const events: string[] = [];
  const token = mintTokenWith(state, now, role, ttlSecs);
  if (state.currentToken) pushHistory(state, state.currentToken);
  state.currentToken = token;
  state.lastRotationAt = now;
  events.push(`TOKEN GENERATED: jti=${token.payload.jti} role=${role} ttl=${ttlSecs}s exp=${iso(token.expiresAt)}`);
  return { state, token, events };
}

/** Parses a JWT into header / payload. Mirrors the Decode subcommand. */
export function decodeToken(raw: string): { header: JwtHeader; payload: JwtPayload; signature: string } {
  const parts = raw.split(".");
  if (parts.length !== 3) {
    throw new Error(`Invalid JWT: expected 3 dot-separated parts, got ${parts.length}`);
  }
  const header = JSON.parse(b64urlDecodeUtf8(parts[0])) as JwtHeader;
  const payload = JSON.parse(b64urlDecodeUtf8(parts[1])) as JwtPayload;
  return { header, payload, signature: parts[2] };
}

/** Verifies a JWT. For the demo we recompute the mock signature and compare. */
export function verifyToken(raw: string, now: number): { ok: boolean; reason: string; payload?: JwtPayload } {
  let decoded;
  try {
    decoded = decodeToken(raw);
  } catch (e) {
    return { ok: false, reason: (e as Error).message };
  }
  const { header, payload, signature } = decoded;
  const expected = mockSign(`${b64urlEncodeUtf8(JSON.stringify(header))}.${b64urlEncodeUtf8(JSON.stringify(payload))}`);
  if (expected !== signature) return { ok: false, reason: "signature mismatch", payload };
  if (typeof payload.exp !== "number") return { ok: false, reason: "missing exp claim", payload };
  if (now > payload.exp * 1000) return { ok: false, reason: `expired at ${iso(payload.exp * 1000)}`, payload };
  return { ok: true, reason: "signature valid & not expired", payload };
}

/** Extracts the `sub` claim. Mirrors the UserId subcommand. */
export function userIdOf(raw: string): string {
  return decodeToken(raw).payload.sub;
}

// ---------- internals ----------

function mintToken(state: AuthState, now: number): JwtToken {
  return mintTokenWith(state, now, state.config.role, state.config.ttlSecs);
}

function mintTokenWith(state: AuthState, now: number, role: string, ttlSecs: number): JwtToken {
  const iatSec = Math.floor(now / 1000);
  const expSec = iatSec + Math.max(1, Math.floor(ttlSecs));
  const jti = shortId(now);
  const header: JwtHeader = { alg: "ES256", typ: "JWT" };
  const payload: JwtPayload = { sub: SUBJECT, role, iat: iatSec, exp: expSec, jti };
  const h = b64urlEncodeUtf8(JSON.stringify(header));
  const p = b64urlEncodeUtf8(JSON.stringify(payload));
  const signature = mockSign(`${h}.${p}`);
  const raw = `${h}.${p}.${signature}`;
  state.stats.generated += 1;
  return {
    header,
    payload,
    signature,
    raw,
    issuedAt: now,
    expiresAt: expSec * 1000,
    verified: true,
  };
}

function pushHistory(state: AuthState, token: JwtToken): void {
  state.history.push(token);
  while (state.history.length > MAX_HISTORY) state.history.shift();
}

function mockSign(input: string): string {
  // Deterministic 43-char base64url from FNV-1a — no cryptographic value, but
  // stable enough that decode-then-verify round-trips inside the demo.
  const salt = "silvana-auth-demo-secret";
  const buf = new Uint8Array(32);
  let h = 2166136261 >>> 0;
  const src = input + "|" + salt;
  for (let i = 0; i < 32; i++) {
    for (let j = 0; j < src.length; j++) {
      h ^= src.charCodeAt(j);
      h = Math.imul(h, 16777619) >>> 0;
    }
    h ^= i * 2654435761;
    h = Math.imul(h, 16777619) >>> 0;
    buf[i] = h & 0xff;
  }
  return b64urlEncodeBytes(buf);
}

function shortId(now: number): string {
  const rand = Math.floor(Math.random() * 0xffffff).toString(16).padStart(6, "0");
  return `${now.toString(36)}-${rand}`;
}

function iso(epochMs: number): string {
  return new Date(epochMs).toISOString();
}

// --- base64url helpers that work in both Node and browser contexts ---

function b64urlEncodeUtf8(s: string): string {
  const bytes = new TextEncoder().encode(s);
  return b64urlEncodeBytes(bytes);
}

function b64urlDecodeUtf8(s: string): string {
  const bytes = b64urlDecodeBytes(s);
  return new TextDecoder().decode(bytes);
}

function b64urlEncodeBytes(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  const std = typeof btoa !== "undefined"
    ? btoa(bin)
    : Buffer.from(bytes).toString("base64");
  return std.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function b64urlDecodeBytes(s: string): Uint8Array {
  const pad = s.length % 4 === 0 ? 0 : 4 - (s.length % 4);
  const std = s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat(pad);
  if (typeof atob !== "undefined") {
    const bin = atob(std);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  return new Uint8Array(Buffer.from(std, "base64"));
}
