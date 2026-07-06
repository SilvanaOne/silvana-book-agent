// Port of agent-copy-trading. Simulates a live leader-order stream and
// mirrors every order.created onto the follower's book at a configurable
// scale, filtered by market and capped by notional. Mirrors are rejected
// by rule when a filter or cap fires — each rejection is counted.

export type Side = "BID" | "OFFER";

export type LeaderOrder = Readonly<{
  seq: number;
  t: number;
  market: string;
  side: Side;
  price: number;
  quantity: number;
}>;

export type Mirror = Readonly<{
  seq: number;
  t: number;
  leaderSeq: number;
  market: string;
  side: Side;
  price: number;
  leaderQty: number;
  mirrorQty: number;
  leaderNotional: number;
  mirrorNotional: number;
}>;

export type Rejection = Readonly<{
  seq: number;
  t: number;
  leaderSeq: number;
  market: string;
  side: Side;
  reason: string;
}>;

export type CopyConfig = Readonly<{
  leader: string;
  follower: string;
  scale: number;
  markets: string[];               // empty = all
  maxLeaderNotional?: number;
  maxMirrorNotional?: number;
  leaderRatePerSec: number;
  marketPool: string[];
  dryRun: boolean;
}>;

export type CopyState = {
  config: CopyConfig;
  status: "running" | "idle";
  nextLeaderSeq: number;
  nextMirrorSeq: number;
  leaderOrders: LeaderOrder[];     // bounded ~120
  mirrors: Mirror[];               // bounded ~120
  rejections: Rejection[];         // bounded ~40
  totalLeader: number;
  totalMirrored: number;
  totalRejected: number;
  refusedByRule: Record<string, number>;
  leaderPosByMarket: Record<string, number>;   // signed leader net position (per market)
  followerPosByMarket: Record<string, number>; // signed follower net position
  nextSpawnAt: number;
};

const MAX_LEADER = 120;
const MAX_MIRRORS = 120;
const MAX_REJ = 40;

export function initState(config: CopyConfig, now: number): CopyState {
  return {
    config,
    status: "running",
    nextLeaderSeq: 1,
    nextMirrorSeq: 1,
    leaderOrders: [],
    mirrors: [],
    rejections: [],
    totalLeader: 0,
    totalMirrored: 0,
    totalRejected: 0,
    refusedByRule: {},
    leaderPosByMarket: {},
    followerPosByMarket: {},
    nextSpawnAt: now,
  };
}

export function step(state: CopyState, _price: number, now: number): { state: CopyState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];
  const interval = 1000 / Math.max(0.05, state.config.leaderRatePerSec);
  while (now >= state.nextSpawnAt) {
    const order = spawnLeader(state, state.nextSpawnAt);
    state.nextSpawnAt += interval;
    state.leaderOrders.push(order);
    while (state.leaderOrders.length > MAX_LEADER) state.leaderOrders.shift();
    state.totalLeader += 1;

    const sign = order.side === "BID" ? 1 : -1;
    state.leaderPosByMarket[order.market] = (state.leaderPosByMarket[order.market] ?? 0) + sign * order.quantity;

    const decision = decide(state, order, now);
    if (decision.mirror) {
      const m = decision.mirror;
      state.mirrors.push(m);
      while (state.mirrors.length > MAX_MIRRORS) state.mirrors.shift();
      state.totalMirrored += 1;
      state.followerPosByMarket[order.market] = (state.followerPosByMarket[order.market] ?? 0) + sign * m.mirrorQty;
      log.push(`MIRROR leader#${order.seq} → mirror#${m.seq} ${m.side} ${m.market} qty=${m.mirrorQty.toFixed(4)} @${m.price.toFixed(4)}`);
    } else if (decision.rejection) {
      state.rejections.push(decision.rejection);
      while (state.rejections.length > MAX_REJ) state.rejections.shift();
      state.totalRejected += 1;
      state.refusedByRule[decision.rejection.reason] = (state.refusedByRule[decision.rejection.reason] ?? 0) + 1;
      log.push(`REFUSE leader#${order.seq} ${order.market} ${order.side} — ${decision.rejection.reason}`);
    }
  }
  return { state, log };
}

function spawnLeader(state: CopyState, t: number): LeaderOrder {
  const market = state.config.marketPool[Math.floor(Math.random() * state.config.marketPool.length)];
  const seq = state.nextLeaderSeq++;
  const side: Side = Math.random() < 0.5 ? "BID" : "OFFER";
  const price = Math.round((0.5 + Math.random() * 200) * 10000) / 10000;
  const quantity = Math.round((0.5 + Math.random() * 40) * 100) / 100;
  return { seq, t, market, side, price, quantity };
}

function decide(state: CopyState, o: LeaderOrder, now: number): { mirror: Mirror | null; rejection: Rejection | null } {
  const c = state.config;
  const leaderNotional = o.price * o.quantity;
  const markets = c.markets;
  if (markets.length > 0 && !markets.includes(o.market)) {
    return { mirror: null, rejection: reject(state, o, now, "market_filter") };
  }
  if (c.maxLeaderNotional !== undefined && leaderNotional > c.maxLeaderNotional) {
    return { mirror: null, rejection: reject(state, o, now, "max_leader_notional") };
  }
  const mirrorQty = o.quantity * c.scale;
  const mirrorNotional = o.price * mirrorQty;
  if (c.maxMirrorNotional !== undefined && mirrorNotional > c.maxMirrorNotional) {
    return { mirror: null, rejection: reject(state, o, now, "max_mirror_notional") };
  }
  if (mirrorQty <= 0 || o.price <= 0) {
    return { mirror: null, rejection: reject(state, o, now, "degenerate") };
  }
  const m: Mirror = {
    seq: state.nextMirrorSeq++,
    t: now,
    leaderSeq: o.seq,
    market: o.market,
    side: o.side,
    price: o.price,
    leaderQty: o.quantity,
    mirrorQty,
    leaderNotional,
    mirrorNotional,
  };
  return { mirror: m, rejection: null };
}

function reject(state: CopyState, o: LeaderOrder, now: number, reason: string): Rejection {
  return { seq: state.nextMirrorSeq++, t: now, leaderSeq: o.seq, market: o.market, side: o.side, reason };
}
