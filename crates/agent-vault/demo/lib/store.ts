// Singleton in-memory state for the vault demo. Resets on server restart.
// Keeps a tiny "vault" model: users, sessions, and per-user secret slots.

import { generateKeyPairSync, randomBytes } from "node:crypto";

export type SecretKind = "wallet" | "cex";
export type WalletChain = "canton";
export type CexVenue = "bybit" | "kucoin";

export type SecretSlot = {
  id: string;
  kind: SecretKind;
  label: string | null;
  createdAt: number;
  // wallet only
  chain?: WalletChain;
  partyId?: string;
  publicKeyHex?: string; // synthetic, derived on save
  // cex only
  venue?: CexVenue;
};

export type User = {
  id: string;              // uuid v4
  username: string;
  email: string;
  createdAt: number;
  slots: SecretSlot[];
};

export type Session = {
  token: string;
  userId: string;
  expiresAt: number;
};

type VaultKey = {
  keyId: string;
  pem: string;             // SPKI public key PEM
};

type Store = {
  users: Map<string, User>;         // by userId
  usersByLogin: Map<string, string>; // by lowercased username|email → userId
  sessions: Map<string, Session>;    // by token
  key: VaultKey;
};

declare global {
  // eslint-disable-next-line no-var
  var __VAULT_DEMO_STORE__: Store | undefined;
}

function makeVaultKey(): VaultKey {
  const { publicKey } = generateKeyPairSync("rsa", {
    modulusLength: 2048,
    publicKeyEncoding: { type: "spki", format: "pem" },
    privateKeyEncoding: { type: "pkcs8", format: "pem" },
  });
  return { keyId: "demo-" + randomBytes(4).toString("hex"), pem: publicKey };
}

function createStore(): Store {
  return {
    users: new Map(),
    usersByLogin: new Map(),
    sessions: new Map(),
    key: makeVaultKey(),
  };
}

export function store(): Store {
  if (!globalThis.__VAULT_DEMO_STORE__) {
    globalThis.__VAULT_DEMO_STORE__ = createStore();
  }
  return globalThis.__VAULT_DEMO_STORE__;
}

const SESSION_TTL_MS = 12 * 60 * 60 * 1000;

export function newSession(userId: string): Session {
  const s = store();
  const token = randomBytes(32).toString("hex");
  const sess: Session = {
    token,
    userId,
    expiresAt: Date.now() + SESSION_TTL_MS,
  };
  s.sessions.set(token, sess);
  return sess;
}

export function readSession(token: string | undefined): Session | null {
  if (!token) return null;
  const sess = store().sessions.get(token);
  if (!sess) return null;
  if (sess.expiresAt < Date.now()) {
    store().sessions.delete(token);
    return null;
  }
  return sess;
}

export function dropSession(token: string | undefined): void {
  if (!token) return;
  store().sessions.delete(token);
}

export function findUser(userId: string): User | null {
  return store().users.get(userId) ?? null;
}

export function upsertUser(input: {
  username?: string;
  email?: string;
}): User {
  const s = store();
  const username = (input.username ?? "").trim();
  const email = (input.email ?? "").trim();
  const loginKey = (username || email).toLowerCase();
  const existingId = s.usersByLogin.get(loginKey);
  if (existingId) {
    const u = s.users.get(existingId);
    if (u) return u;
  }
  const id = cryptoUuid();
  const user: User = {
    id,
    username: username || email.split("@")[0] || "tenant",
    email: email || `${id.slice(0, 8)}@demo.silvana`,
    createdAt: Date.now(),
    slots: [],
  };
  s.users.set(id, user);
  if (username) s.usersByLogin.set(username.toLowerCase(), id);
  if (email) s.usersByLogin.set(email.toLowerCase(), id);
  return user;
}

export function loginOrCreate(loginOrEmail: string): User {
  const s = store();
  const key = loginOrEmail.trim().toLowerCase();
  const existingId = s.usersByLogin.get(key);
  if (existingId) {
    const u = s.users.get(existingId);
    if (u) return u;
  }
  return upsertUser(key.includes("@") ? { email: key } : { username: key });
}

export function addWalletSlot(
  userId: string,
  input: { label: string; partyId?: string },
): SecretSlot | null {
  const u = findUser(userId);
  if (!u) return null;
  const slot: SecretSlot = {
    id: "w-" + randomBytes(6).toString("hex"),
    kind: "wallet",
    chain: "canton",
    label: input.label || null,
    partyId: input.partyId || undefined,
    publicKeyHex: randomBytes(32).toString("hex"),
    createdAt: Date.now(),
  };
  u.slots.push(slot);
  return slot;
}

export function addCexSlot(
  userId: string,
  input: { venue: CexVenue; label?: string },
): SecretSlot | null {
  const u = findUser(userId);
  if (!u) return null;
  const slot: SecretSlot = {
    id: "c-" + randomBytes(6).toString("hex"),
    kind: "cex",
    venue: input.venue,
    label: input.label || null,
    createdAt: Date.now(),
  };
  u.slots.push(slot);
  return slot;
}

export function deleteSlot(userId: string, slotId: string): boolean {
  const u = findUser(userId);
  if (!u) return false;
  const before = u.slots.length;
  u.slots = u.slots.filter((s) => s.id !== slotId);
  return u.slots.length < before;
}

function cryptoUuid(): string {
  // RFC 4122 v4 via node:crypto
  const b = randomBytes(16);
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const hex = b.toString("hex");
  return (
    hex.slice(0, 8) +
    "-" +
    hex.slice(8, 12) +
    "-" +
    hex.slice(12, 16) +
    "-" +
    hex.slice(16, 20) +
    "-" +
    hex.slice(20)
  );
}

// -- balances ------------------------------------------------------------

export type BalanceRow = {
  sourceId: string;
  venueOrChain: string;
  asset: string;
  free: number;
  locked: number;
  usd: number | null;
  address?: string;
};

export type BalanceSource = {
  id: string;
  kind: "canton" | "cex";
  label: string;
  totalUsd: number | null;
  error?: string;
};

export type BalancesSnapshot = {
  totalUsd: number | null;
  updatedAt: number;
  sources: BalanceSource[];
  rows: BalanceRow[];
};

// Canned balances per slot — deterministic so refresh shows stable numbers.
export function balancesFor(userId: string): BalancesSnapshot {
  const u = findUser(userId);
  const now = Date.now();
  if (!u) {
    return { totalUsd: 0, updatedAt: now, sources: [], rows: [] };
  }
  const sources: BalanceSource[] = [];
  const rows: BalanceRow[] = [];
  let total = 0;
  for (const s of u.slots) {
    if (s.kind === "wallet") {
      const label = s.label || "canton wallet";
      const cc = 42.5 + (hashSeed(s.id) % 500) / 10;
      const usdc = 120 + (hashSeed(s.id + "u") % 4000) / 10;
      const usd = cc * 0.85 + usdc * 1;
      sources.push({
        id: s.id,
        kind: "canton",
        label,
        totalUsd: round2(usd),
      });
      const addr = s.partyId || short(s.publicKeyHex || s.id);
      rows.push({
        sourceId: s.id,
        venueOrChain: "canton",
        asset: "CC",
        free: round4(cc),
        locked: 0,
        usd: round2(cc * 0.85),
        address: addr,
      });
      rows.push({
        sourceId: s.id,
        venueOrChain: "canton",
        asset: "USDC",
        free: round2(usdc),
        locked: 0,
        usd: round2(usdc),
        address: addr,
      });
      total += usd;
    } else if (s.kind === "cex") {
      const label = s.label || `${s.venue} account`;
      const btc = 0.05 + (hashSeed(s.id) % 200) / 10000;
      const eth = 0.8 + (hashSeed(s.id + "e") % 500) / 100;
      const usdt = 250 + (hashSeed(s.id + "t") % 8000) / 10;
      const btcUsd = btc * 62500;
      const ethUsd = eth * 3100;
      const usdtUsd = usdt;
      const usd = btcUsd + ethUsd + usdtUsd;
      sources.push({
        id: s.id,
        kind: "cex",
        label,
        totalUsd: round2(usd),
      });
      rows.push({
        sourceId: s.id,
        venueOrChain: s.venue || "cex",
        asset: "BTC",
        free: round4(btc),
        locked: 0,
        usd: round2(btcUsd),
      });
      rows.push({
        sourceId: s.id,
        venueOrChain: s.venue || "cex",
        asset: "ETH",
        free: round4(eth),
        locked: 0,
        usd: round2(ethUsd),
      });
      rows.push({
        sourceId: s.id,
        venueOrChain: s.venue || "cex",
        asset: "USDT",
        free: round2(usdt),
        locked: 0,
        usd: round2(usdtUsd),
      });
      total += usd;
    }
  }
  return {
    totalUsd: sources.length ? round2(total) : null,
    updatedAt: now,
    sources,
    rows,
  };
}

function hashSeed(s: string): number {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619) >>> 0;
  }
  return h;
}
function round2(n: number): number {
  return Math.round(n * 100) / 100;
}
function round4(n: number): number {
  return Math.round(n * 10000) / 10000;
}
function short(s: string): string {
  return s.length > 12 ? s.slice(0, 8) + "…" + s.slice(-4) : s;
}
