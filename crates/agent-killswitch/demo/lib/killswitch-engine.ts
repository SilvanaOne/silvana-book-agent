// Port of agent-killswitch to TypeScript for the interactive demo.
// Mirrors crates/agent-killswitch/src/main.rs (subcommand `run`).
//
// Two triggers:
//   * open orders count exceeds `maxOpenOrders`
//   * failed settlements exceed `maxFailedSettlements`
// When either fires, the killswitch TRIPS: status="tripped", all open orders
// are cancelled (accounted for by ordersCancelled), and no further mutation
// happens.
//
// Because the demo has no real orderbook, we simulate:
//   * open orders count grows by `orderGrowthPerTick` per tick
//   * each tick has probability `failureRatePerTick` of a new failed
//     settlement.
// The health check runs every `checkIntervalSecs` seconds (ticks).

export type KillswitchStatus = "monitoring" | "tripped" | "idle";

export type KillswitchConfig = Readonly<{
  maxOpenOrders: number;
  maxFailedSettlements: number;
  checkIntervalSecs: number;
  orderGrowthPerTick: number;
  failureRatePerTick: number;   // in [0,1]
  startingOpenOrders: number;
  startingFailed: number;
}>;

export type KillswitchState = {
  config: KillswitchConfig;
  status: KillswitchStatus;
  openOrders: number;
  openOrdersFrac: number;       // accumulator for fractional growth
  failedSettlements: number;
  ticksElapsed: number;
  lastCheckAt: number | null;
  breaches: string[];
  tripTimestamp: number | null;
  ordersCancelled: number | null;
  startedAt: number;
  currentPrice: number;         // present only so shared DemoTools compiles
};

export function initState(config: KillswitchConfig, now: number): KillswitchState {
  return {
    config,
    status: "monitoring",
    openOrders: Math.max(0, Math.floor(config.startingOpenOrders)),
    openOrdersFrac: 0,
    failedSettlements: Math.max(0, Math.floor(config.startingFailed)),
    ticksElapsed: 0,
    lastCheckAt: null,
    breaches: [],
    tripTimestamp: null,
    ordersCancelled: null,
    startedAt: now,
    currentPrice: 1,
  };
}

/**
 * Advance the killswitch state by one 1-second tick.
 * `price` is unused — kept for signature compatibility with the demo's
 * generic tick loop.
 */
export function step(
  state: KillswitchState,
  _price: number,
  now: number,
): { state: KillswitchState; events: string[] } {
  if (state.status !== "monitoring") return { state, events: [] };
  const events: string[] = [];

  state.ticksElapsed += 1;

  // Grow open orders (support fractional growth via accumulator).
  state.openOrdersFrac += state.config.orderGrowthPerTick;
  const grown = Math.floor(state.openOrdersFrac);
  if (grown > 0) {
    state.openOrders += grown;
    state.openOrdersFrac -= grown;
  }

  // Roll for a new failed settlement.
  if (Math.random() < state.config.failureRatePerTick) {
    state.failedSettlements += 1;
    events.push(
      `FAILED settlement observed (total=${state.failedSettlements}, limit=${state.config.maxFailedSettlements})`,
    );
  }

  // Health check on schedule.
  if (state.ticksElapsed % Math.max(1, Math.floor(state.config.checkIntervalSecs)) === 0) {
    state.lastCheckAt = now;
    const reasons: string[] = [];
    if (state.openOrders > state.config.maxOpenOrders) {
      reasons.push(`open orders ${state.openOrders} exceeded limit ${state.config.maxOpenOrders}`);
    }
    if (state.failedSettlements > state.config.maxFailedSettlements) {
      reasons.push(
        `failed settlements ${state.failedSettlements} exceeded limit ${state.config.maxFailedSettlements}`,
      );
    }
    if (reasons.length > 0) {
      state.status = "tripped";
      state.breaches = reasons;
      state.tripTimestamp = now;
      state.ordersCancelled = state.openOrders;
      events.push(`TRIP: ${reasons.join("; ")}. Cancel-all initiated.`);
      events.push(`cancel-all complete: ${state.ordersCancelled} orders affected (dry_run=false)`);
    } else {
      events.push(
        `eval: open_orders=${state.openOrders} limit=${state.config.maxOpenOrders}, failed=${state.failedSettlements} limit=${state.config.maxFailedSettlements}`,
      );
    }
  }

  return { state, events };
}

/** Trigger PANIC mode: cancel all + set status to tripped. */
export function panic(state: KillswitchState, now: number, reason = "operator panic"): { state: KillswitchState; events: string[] } {
  if (state.status === "tripped") {
    return { state, events: [`PANIC ignored — already tripped`] };
  }
  const cancelled = state.openOrders;
  state.status = "tripped";
  state.breaches = [reason];
  state.tripTimestamp = now;
  state.ordersCancelled = cancelled;
  const events = [
    `PANIC: ${reason}. Cancel-all initiated.`,
    `cancel-all complete: ${cancelled} orders affected (dry_run=false)`,
  ];
  return { state, events };
}
