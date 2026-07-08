// Port of agent-cash-buffer-push-only band-keeping logic to TypeScript. Mirrors
// crates/agent-cash-buffer-push-only/src/main.rs (buffer_loop).
//
// Rule (per check interval):
//   inflow simulated as balance += incomeRate per tick
//   if balance > max_cc → push (balance − target) to sink via TransferCc
//   if balance < min_cc → warning (agent cannot pull)
//   target = (min_cc + max_cc) / 2

export type CashBufferConfig = Readonly<{
  minCc: number;
  maxCc: number;
  sinkParty: string;
  checkIntervalSecs: number;
  incomeRate: number;      // simulated inflow per tick (CC)
  startingBalance: number;
}>;

export type CashBufferTransfer = Readonly<{
  seq: number;
  t: number;               // epoch ms
  amount: number;          // CC pushed
  fromBalance: number;
  toBalance: number;
  sink: string;
}>;

export type CashBufferWarning = Readonly<{
  seq: number;
  t: number;
  balance: number;
}>;

export type CashBufferState = {
  config: CashBufferConfig;
  status: "monitoring" | "idle";
  currentBalance: number;
  currentPrice: number;      // kept for DemoTools compat; not used by cash-buffer logic
  target: number;
  lastCheckAt: number | null;
  ticksSinceCheck: number;
  pushesCount: number;
  totalPushed: number;
  lastPushAmount: number | null;
  lastPushAt: number | null;
  warningsCount: number;
  lastWarningAt: number | null;
  transfers: CashBufferTransfer[];
  warnings: CashBufferWarning[];
};

const MAX_TRANSFERS = 60;
const MAX_WARNINGS = 60;

export function initState(config: CashBufferConfig): CashBufferState {
  return {
    config,
    status: "monitoring",
    currentBalance: config.startingBalance,
    currentPrice: 1,
    target: (config.minCc + config.maxCc) / 2,
    lastCheckAt: null,
    ticksSinceCheck: 0,
    pushesCount: 0,
    totalPushed: 0,
    lastPushAmount: null,
    lastPushAt: null,
    warningsCount: 0,
    lastWarningAt: null,
    transfers: [],
    warnings: [],
  };
}

/** Applies a tick. `price` is passed through (for DemoTools/chart price), but the
 * cash-buffer logic ignores it — inflow is a fixed per-tick rate. */
export function step(state: CashBufferState, price: number, now: number): { state: CashBufferState; events: string[] } {
  if (state.status !== "monitoring") return { state, events: [] };
  const events: string[] = [];

  if (price > 0) state.currentPrice = price;
  // Simulate inflow every tick.
  state.currentBalance = round8(state.currentBalance + state.config.incomeRate);
  state.ticksSinceCheck += 1;

  // Buffer check runs every checkIntervalSecs (1 tick = 1 sec in this demo).
  if (state.ticksSinceCheck < state.config.checkIntervalSecs) {
    return { state, events };
  }
  state.ticksSinceCheck = 0;
  state.lastCheckAt = now;

  const { minCc, maxCc, sinkParty } = state.config;
  const balance = state.currentBalance;

  if (balance > maxCc) {
    const excess = round8(balance - state.target);
    if (excess > 0) {
      const from = balance;
      const to = round8(balance - excess);
      state.currentBalance = to;
      const seq = state.pushesCount + 1;
      const transfer: CashBufferTransfer = {
        seq,
        t: now,
        amount: excess,
        fromBalance: from,
        toBalance: to,
        sink: sinkParty,
      };
      state.transfers.push(transfer);
      if (state.transfers.length > MAX_TRANSFERS) state.transfers.shift();
      state.pushesCount = seq;
      state.totalPushed = round8(state.totalPushed + excess);
      state.lastPushAmount = excess;
      state.lastPushAt = now;
      events.push(
        `PUSH #${seq}: ${fmt(excess)} CC → ${sinkParty} (balance ${fmt(from)}→${fmt(to)}, target ${fmt(state.target)})`,
      );
    }
  } else if (balance < minCc) {
    const seq = state.warningsCount + 1;
    state.warnings.push({ seq, t: now, balance });
    if (state.warnings.length > MAX_WARNINGS) state.warnings.shift();
    state.warningsCount = seq;
    state.lastWarningAt = now;
    events.push(
      `LOW BALANCE #${seq}: ${fmt(balance)} CC < min_cc ${fmt(minCc)} — agent cannot pull, please refill`,
    );
  } else {
    events.push(
      `CHECK: balance ${fmt(balance)} CC inside band [${fmt(minCc)}, ${fmt(maxCc)}]`,
    );
  }

  return { state, events };
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
