// Port of agent-failure-recovery sweep logic to TypeScript. Mirrors
// crates/agent-failure-recovery/src/main.rs (fn sweep).
//
// Model:
//   Each tick may spawn new settlement Proposals into pending state.
//   Some pending proposals eventually resolve (settled / failed); others
//   get stuck. Every checkIntervalSecs the watchdog sweeps the list and
//   surfaces PENDING older than maxPendingAgeSecs, plus any FAILED.
//   With cancelRelatedOrders, related active orders are cancelled
//   (or logged in dryRun).

export type ProposalStatus = "pending" | "settled" | "failed";
export type OrderStatus = "active" | "cancelled" | "filled";
export type OrderSide = "BID" | "OFFER";

export type Proposal = {
  id: number;
  createdAt: number;         // epoch ms
  status: ProposalStatus;
  ageSec: number;
  notional: number;
  bidOrderId?: number;
  offerOrderId?: number;
  resolvedAt?: number;       // when it moved out of pending
  stuck: boolean;            // rolled at creation — stuck ones never resolve
  staleFlagged: boolean;     // has the sweeper already surfaced it
  failedReported: boolean;
};

export type RelatedOrder = {
  orderId: number;
  proposalId: number;
  market: string;
  side: OrderSide;
  qty: number;
  status: OrderStatus;
};

export type FailureRecoveryConfig = Readonly<{
  maxPendingAgeSecs: number;
  checkIntervalSecs: number;
  cancelRelatedOrders: boolean;
  dryRun: boolean;
  proposalArrivalPerTick: number;   // Poisson-ish arrival rate
  pendingStuckProbability: number;  // p(new proposal is stuck)
  failureProbability: number;       // p(a resolving proposal fails)
  startingPrice: number;
}>;

export type FailureRecoveryState = {
  config: FailureRecoveryConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  proposals: Proposal[];       // recent, bounded ~50
  relatedOrders: RelatedOrder[];
  staleFlaggedCount: number;
  cancelledCount: number;
  failedFoundCount: number;
  sweepsCount: number;
  newProposalsCount: number;
  settledCount: number;
  failedCount: number;
  lastSweepAt: number | null;
  nextProposalId: number;
  nextOrderId: number;
  tickCount: number;
  lastSweepTick: number;
};

const MAX_PROPOSALS = 50;
const MAX_RELATED_ORDERS = 120;

export function initState(config: FailureRecoveryConfig, startPrice: number): FailureRecoveryState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    proposals: [],
    relatedOrders: [],
    staleFlaggedCount: 0,
    cancelledCount: 0,
    failedFoundCount: 0,
    sweepsCount: 0,
    newProposalsCount: 0,
    settledCount: 0,
    failedCount: 0,
    lastSweepAt: null,
    nextProposalId: 1001,
    nextOrderId: 5001,
    tickCount: 0,
    lastSweepTick: 0,
  };
}

const TICK_SECS = 1;

export function step(state: FailureRecoveryState, price: number, now: number): { state: FailureRecoveryState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.tickCount += 1;

  // 1) Age all proposals + resolve non-stuck pending ones stochastically.
  for (const p of state.proposals) {
    if (p.status === "pending") {
      p.ageSec += TICK_SECS;
      if (!p.stuck) {
        // Poisson-like resolution: each tick, ~15% chance to resolve.
        if (Math.random() < 0.15) {
          if (Math.random() < state.config.failureProbability * 3) {
            // NB: failureProbability biases outcomes among resolving proposals.
            p.status = "failed";
            p.resolvedAt = now;
            state.failedCount += 1;
          } else {
            p.status = "settled";
            p.resolvedAt = now;
            state.settledCount += 1;
            // Related orders considered filled.
            for (const o of state.relatedOrders) {
              if (o.proposalId === p.id && o.status === "active") {
                o.status = "filled";
              }
            }
          }
        }
      }
    }
  }

  // 2) Arrivals (Bernoulli approx per tick).
  const arrivals = poissonArrivals(state.config.proposalArrivalPerTick);
  for (let i = 0; i < arrivals; i++) {
    const stuck = Math.random() < state.config.pendingStuckProbability;
    const proposalId = state.nextProposalId++;
    const bidOrderId = state.nextOrderId++;
    const offerOrderId = state.nextOrderId++;
    const notional = Number((price * (2 + Math.random() * 8)).toFixed(4));
    state.proposals.push({
      id: proposalId,
      createdAt: now,
      status: "pending",
      ageSec: 0,
      notional,
      bidOrderId,
      offerOrderId,
      stuck,
      staleFlagged: false,
      failedReported: false,
    });
    state.newProposalsCount += 1;
    // 1-2 related orders per proposal.
    state.relatedOrders.push({
      orderId: bidOrderId,
      proposalId,
      market: "CC-USDC",
      side: "BID",
      qty: Number((0.5 + Math.random() * 4).toFixed(3)),
      status: "active",
    });
    if (Math.random() < 0.8) {
      state.relatedOrders.push({
        orderId: offerOrderId,
        proposalId,
        market: "CC-USDC",
        side: "OFFER",
        qty: Number((0.5 + Math.random() * 4).toFixed(3)),
        status: "active",
      });
    }
  }

  // Trim buffers.
  while (state.proposals.length > MAX_PROPOSALS) state.proposals.shift();
  while (state.relatedOrders.length > MAX_RELATED_ORDERS) state.relatedOrders.shift();

  // 3) Sweep at the configured cadence.
  const ticksSinceSweep = state.tickCount - state.lastSweepTick;
  if (ticksSinceSweep >= state.config.checkIntervalSecs) {
    state.lastSweepTick = state.tickCount;
    state.lastSweepAt = now;
    state.sweepsCount += 1;

    const stale: Proposal[] = [];
    const failed: Proposal[] = [];
    for (const p of state.proposals) {
      if (p.status === "pending" && p.ageSec > state.config.maxPendingAgeSecs && !p.staleFlagged) {
        stale.push(p);
      } else if (p.status === "failed" && !p.failedReported) {
        failed.push(p);
      }
    }

    for (const p of stale) {
      p.staleFlagged = true;
      state.staleFlaggedCount += 1;
      events.push(`STALE #${p.id} — age ${p.ageSec}s, notional ${p.notional.toFixed(4)}`);
    }
    for (const p of failed) {
      p.failedReported = true;
      state.failedFoundCount += 1;
      events.push(`FAILED #${p.id} notional ${p.notional.toFixed(4)}`);
    }

    if (state.config.cancelRelatedOrders && (stale.length > 0 || failed.length > 0)) {
      const targetProposalIds = new Set<number>([...stale.map((p) => p.id), ...failed.map((p) => p.id)]);
      for (const o of state.relatedOrders) {
        if (o.status !== "active") continue;
        if (!targetProposalIds.has(o.proposalId)) continue;
        if (state.config.dryRun) {
          events.push(`[DRY-RUN] would cancel order ${o.orderId} (${o.side} ${o.qty} ${o.market})`);
        } else {
          o.status = "cancelled";
          state.cancelledCount += 1;
          events.push(`CANCEL order ${o.orderId} (${o.side} ${o.qty} ${o.market}) — related to #${o.proposalId}`);
        }
      }
    }

    events.push(
      `sweep #${state.sweepsCount}: scanned ${state.proposals.length} proposals — stale=${stale.length} failed=${failed.length}`,
    );
  }

  return { state, events };
}

function poissonArrivals(mean: number): number {
  if (mean <= 0) return 0;
  // For small mean, Bernoulli approximation.
  if (mean < 1) {
    return Math.random() < mean ? 1 : 0;
  }
  // Knuth's Poisson for larger mean.
  const L = Math.exp(-mean);
  let k = 0;
  let p = 1;
  do {
    k += 1;
    p *= Math.random();
  } while (p > L);
  return k - 1;
}
