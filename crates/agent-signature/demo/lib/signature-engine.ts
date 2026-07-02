// Demo port of agent-signature (Ed25519 signing wrapper). Mirrors the CLI
// surface in crates/agent-signature/src/main.rs but uses a *mocked*
// deterministic signature scheme so the demo needs no native crypto binding.
//
// The real agent uses ed25519-dalek. For teaching purposes this demo builds a
// stable 64-byte "signature" by mixing a 32-byte private key with SHA-256-ish
// FNV/DJB2 hashes of the message. It is NOT cryptographically secure — the
// only property preserved is: same (key, message) → same sig; tampered
// message or sig → verify() returns false.

export type OpKind = "sign-raw" | "sign-canonical" | "verify" | "gen-key";

export type SignOperation = Readonly<{
  seq: number;
  t: number;
  kind: OpKind;
  input: string;        // human-readable message (truncated for display)
  output: string;       // signature base64 or "OK"/"FAIL" for verify
  verified?: boolean;   // present only for kind === "verify"
  publicKey?: string;   // base64url — recorded for key generations
  privateKeyMasked?: string; // masked ellipsis of new private key on gen-key
  tampered?: boolean;   // true if this verify used a tampered signature
}>;

export type SignatureConfig = Readonly<{
  autoDemoIntervalSecs: number; // auto-emit one sign+verify roundtrip every N secs
  tamperRate: number;           // 0..1 — chance a verify uses a tampered sig
  startingPrice: number;        // seeds the price sim (used inside demo messages)
}>;

export type SignatureStats = {
  signed: number;
  verified: number;
  verifiedOk: number;
  verifiedFailed: number;
  keyGenerated: number;
};

export type SignatureState = {
  config: SignatureConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  publicKey: string;              // base64url (RFC 8037 x)
  privateKeyMasked: string;       // masked ellipsis, never full material
  privateKey: string;             // raw hex — kept server-side, never sent to client
  operations: SignOperation[];    // recent ops, bounded
  stats: SignatureStats;
  lastAutoTickAt: number;         // ms of last auto-demo emission
  seq: number;                    // monotonic operation counter
};

const MAX_OPS = 40;
const ALGO = "Ed25519 (mock)";

/* -----------------------------------------------------------------------
 * Deterministic pseudo-crypto (demo only)
 * ----------------------------------------------------------------------- */

function fnv1a32(s: string): number {
  let h = 0x811c9dc5 >>> 0;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

function djb2(s: string): number {
  let h = 5381 >>> 0;
  for (let i = 0; i < s.length; i++) {
    h = (Math.imul(h, 33) + s.charCodeAt(i)) >>> 0;
  }
  return h >>> 0;
}

function toBytes32(seed: string): number[] {
  const out = new Array<number>(32);
  for (let i = 0; i < 32; i++) {
    const mix = fnv1a32(seed + ":" + i) ^ djb2(seed + "#" + i);
    out[i] = mix & 0xff;
  }
  return out;
}

function b64(bytes: number[]): string {
  // Standard Base64 (with padding) — matches CLI `signature_b64`.
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  // btoa is available in Node 18+ / browsers. Fallback: Buffer if present.
  if (typeof btoa === "function") return btoa(bin);
  const B = (globalThis as unknown as { Buffer?: { from: (s: string, enc: string) => { toString: (enc: string) => string } } }).Buffer;
  if (B) return B.from(bin, "binary").toString("base64");
  throw new Error("no base64 encoder available");
}

function b64url(bytes: number[]): string {
  return b64(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function b64ToBytes(s: string): number[] {
  const bin =
    typeof atob === "function"
      ? atob(s)
      : ((globalThis as unknown as { Buffer?: { from: (s: string, enc: string) => { toString: (enc: string) => string } } }).Buffer?.from(s, "base64").toString("binary") ?? "");
  const out: number[] = new Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function hex(bytes: number[]): string {
  return bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
}

function newPrivateKey(): { priv: string; pub: string; masked: string } {
  const seed = String(Date.now()) + ":" + String(Math.random());
  const priv = toBytes32(seed);
  // Public key is a deterministic hash of the private key — again, demo only.
  const pub = toBytes32("pub:" + hex(priv));
  const privHex = hex(priv);
  const masked = privHex.slice(0, 8) + "…" + privHex.slice(-4);
  return { priv: privHex, pub: b64url(pub), masked };
}

/** Deterministic 64-byte "signature" derived from key + message. */
function mockSign(privHex: string, message: string): string {
  const sigBytes: number[] = new Array(64);
  for (let i = 0; i < 64; i++) {
    const mix = fnv1a32(privHex + "|" + message + "|" + i) ^ djb2(message + "#" + privHex + "@" + i);
    sigBytes[i] = mix & 0xff;
  }
  return b64(sigBytes);
}

function mockVerify(pubB64url: string, message: string, sigB64: string, privHex: string): boolean {
  // We derive the "expected" signature the same way and compare bytes.
  // This is what re-signing gives us — a stable check for the demo.
  void pubB64url;
  const expected = mockSign(privHex, message);
  return sigB64 === expected;
}

/** Flip one char of a base64 signature — guaranteed different, still valid b64 chars. */
function tamperSignature(sig: string): string {
  if (sig.length === 0) return sig;
  const idx = sig.length - 1 - (sig.endsWith("=") ? 1 : 0);
  const ch = sig[idx];
  const alt = ch === "A" ? "B" : "A";
  return sig.slice(0, Math.max(0, idx)) + alt + sig.slice(idx + 1);
}

/* -----------------------------------------------------------------------
 * State
 * ----------------------------------------------------------------------- */

export function initState(config: SignatureConfig): SignatureState {
  const { priv, pub, masked } = newPrivateKey();
  const now = Date.now();
  const state: SignatureState = {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    publicKey: pub,
    privateKeyMasked: masked,
    privateKey: priv,
    operations: [
      {
        seq: 1,
        t: now,
        kind: "gen-key",
        input: "keypair generated",
        output: pub,
        publicKey: pub,
        privateKeyMasked: masked,
      },
    ],
    stats: { signed: 0, verified: 0, verifiedOk: 0, verifiedFailed: 0, keyGenerated: 1 },
    lastAutoTickAt: 0,
    seq: 1,
  };
  return state;
}

function pushOp(state: SignatureState, op: SignOperation): void {
  state.operations.push(op);
  if (state.operations.length > MAX_OPS) state.operations.shift();
}

/** Apply a tick. Emits an auto sign+verify roundtrip every autoDemoIntervalSecs. */
export function step(state: SignatureState, price: number, now: number): { state: SignatureState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  state.currentPrice = price;
  const events: string[] = [];
  const intervalMs = Math.max(500, state.config.autoDemoIntervalSecs * 1000);

  if (now - state.lastAutoTickAt >= intervalMs) {
    state.lastAutoTickAt = now;

    // 1) sign a synthesized "trade" message
    const seqN = state.stats.signed + 1;
    const message = `trade #${seqN} at price ${fmt(price)}`;
    const sig = mockSign(state.privateKey, message);
    state.seq += 1;
    pushOp(state, {
      seq: state.seq,
      t: now,
      kind: "sign-raw",
      input: message,
      output: sig,
    });
    state.stats.signed += 1;
    events.push(`SIGN #${seqN}: msg="${message}" sig=${short(sig)}…`);

    // 2) verify — with `tamperRate` chance we flip one byte first
    const willTamper = Math.random() < clamp01(state.config.tamperRate);
    const sigForVerify = willTamper ? tamperSignature(sig) : sig;
    const ok = mockVerify(state.publicKey, message, sigForVerify, state.privateKey);
    state.seq += 1;
    pushOp(state, {
      seq: state.seq,
      t: now,
      kind: "verify",
      input: message,
      output: ok ? "OK" : "FAIL",
      verified: ok,
      tampered: willTamper,
    });
    state.stats.verified += 1;
    if (ok) state.stats.verifiedOk += 1;
    else state.stats.verifiedFailed += 1;
    events.push(
      `VERIFY #${seqN}: sig=${short(sigForVerify)}… ${willTamper ? "(tampered) " : ""}→ ${ok ? "OK" : "FAIL"}`,
    );
  }

  return { state, events };
}

/** Manual sign — used by the "Sign now" UI panel. */
export function manualSign(state: SignatureState, message: string, now: number, canonical = false): SignOperation {
  const kind: OpKind = canonical ? "sign-canonical" : "sign-raw";
  const sig = mockSign(state.privateKey, canonical ? "ed25519-sha256-v1|" + message : message);
  state.seq += 1;
  const op: SignOperation = {
    seq: state.seq,
    t: now,
    kind,
    input: message,
    output: sig,
    publicKey: state.publicKey,
  };
  pushOp(state, op);
  state.stats.signed += 1;
  return op;
}

/** Manual verify — used by the "Verify" endpoint. */
export function manualVerify(state: SignatureState, message: string, signature: string, now: number): SignOperation {
  const ok = mockVerify(state.publicKey, message, signature, state.privateKey);
  state.seq += 1;
  const op: SignOperation = {
    seq: state.seq,
    t: now,
    kind: "verify",
    input: message,
    output: ok ? "OK" : "FAIL",
    verified: ok,
  };
  pushOp(state, op);
  state.stats.verified += 1;
  if (ok) state.stats.verifiedOk += 1;
  else state.stats.verifiedFailed += 1;
  return op;
}

/** Regenerate the keypair — invalidates all previous signatures. */
export function rotateKey(state: SignatureState, now: number): SignOperation {
  const { priv, pub, masked } = newPrivateKey();
  state.privateKey = priv;
  state.publicKey = pub;
  state.privateKeyMasked = masked;
  state.stats.keyGenerated += 1;
  state.seq += 1;
  const op: SignOperation = {
    seq: state.seq,
    t: now,
    kind: "gen-key",
    input: "keypair rotated",
    output: pub,
    publicKey: pub,
    privateKeyMasked: masked,
  };
  pushOp(state, op);
  return op;
}

/* -----------------------------------------------------------------------
 * Helpers
 * ----------------------------------------------------------------------- */

export const SIGNATURE_ALGO = ALGO;

function clamp01(x: number): number {
  return x < 0 ? 0 : x > 1 ? 1 : x;
}

function short(s: string): string {
  return s.length > 12 ? s.slice(0, 12) : s;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

// Exported for tests / callers that need to build a signature outside of state.
export function _mockSignForTests(privHex: string, message: string): string {
  return mockSign(privHex, message);
}
export function _b64ToBytesForTests(s: string): number[] {
  return b64ToBytes(s);
}
