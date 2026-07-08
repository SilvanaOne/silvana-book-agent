// Port of agent-contractual-compliance-bilateral. Tracks bilateral contract
// obligations against simulated settlement flow. Each contract has a
// counterparty, market, rolling window (hours), min_notional and/or
// max_notional. As SETTLED events arrive their notional accrues to any
// matching contract; on breach we emit contract.under_floor or
// contract.over_ceiling; periodic contract.status snapshots report the
// running tally.

export type Contract = Readonly<{
  id: string;
  counterparty: string;
  market: string;
  windowHours: number;
  minNotional?: number;
  maxNotional?: number;
  expiresAtISO?: string;    // ISO 8601 date
}>;

export type ContractConfig = Readonly<{
  contracts: Contract[];
  eventRatePerSec: number;
  statusIntervalSecs: number;
  parties: string[];
  markets: string[];
}>;

export type Settlement = Readonly<{
  seq: number;
  t: number;
  counterparty: string;
  market: string;
  notional: number;
}>;

export type ContractSnapshot = Readonly<{
  contract: Contract;
  t: number;
  windowStart: number;
  totalInWindow: number;
  underFloor: boolean;
  overCeiling: boolean;
  expired: boolean;
  entries: Array<{ t: number; notional: number }>;
}>;

export type ContractState = {
  config: ContractConfig;
  status: "running" | "idle";
  cycle: number;
  nextSeq: number;
  settlementsSeen: number;
  under: number;                    // total under_floor events
  over: number;                     // total over_ceiling events
  perContractTallies: Record<string, Array<{ t: number; notional: number }>>;
  snapshots: ContractSnapshot[];    // latest per-contract (aligned with contracts)
  nextSpawnAt: number;
  nextStatusAt: number;
};

export function initState(config: ContractConfig, now: number): ContractState {
  const perContractTallies: Record<string, Array<{ t: number; notional: number }>> = {};
  for (const c of config.contracts) perContractTallies[c.id] = [];
  return {
    config,
    status: "running",
    cycle: 0,
    nextSeq: 1,
    settlementsSeen: 0,
    under: 0,
    over: 0,
    perContractTallies,
    snapshots: [],
    nextSpawnAt: now,
    nextStatusAt: now + config.statusIntervalSecs * 1000,
  };
}

export function step(state: ContractState, _price: number, now: number): { state: ContractState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];

  // Spawn settlements at the target rate.
  const intervalMs = 1000 / Math.max(0.1, state.config.eventRatePerSec);
  while (now >= state.nextSpawnAt) {
    const ev = spawn(state, state.nextSpawnAt);
    state.nextSpawnAt += intervalMs;
    state.settlementsSeen += 1;
    // Match this settlement against every contract with the same counterparty & market.
    for (const c of state.config.contracts) {
      if (c.counterparty !== ev.counterparty) continue;
      if (c.market !== ev.market) continue;
      state.perContractTallies[c.id].push({ t: ev.t, notional: ev.notional });
    }
  }

  // Periodic status snapshot.
  if (now >= state.nextStatusAt) {
    state.cycle += 1;
    state.snapshots = state.config.contracts.map((c) => snapshotContract(state, c, now));
    for (const snap of state.snapshots) {
      if (snap.underFloor) { state.under += 1; log.push(`UNDER  ${snap.contract.id} tally=${snap.totalInWindow.toFixed(2)} < min ${snap.contract.minNotional}`); }
      else if (snap.overCeiling) { state.over += 1; log.push(`OVER   ${snap.contract.id} tally=${snap.totalInWindow.toFixed(2)} > max ${snap.contract.maxNotional}`); }
      else log.push(`OK     ${snap.contract.id} tally=${snap.totalInWindow.toFixed(2)}`);
    }
    state.nextStatusAt = now + state.config.statusIntervalSecs * 1000;
  }

  return { state, log };
}

function spawn(state: ContractState, t: number): Settlement {
  const cp = state.config.parties[Math.floor(Math.random() * state.config.parties.length)];
  const market = state.config.markets[Math.floor(Math.random() * state.config.markets.length)];
  const notional = Math.round((100 + Math.random() * 8000) * 100) / 100;
  const seq = state.nextSeq++;
  return { seq, t, counterparty: cp, market, notional };
}

function snapshotContract(state: ContractState, c: Contract, now: number): ContractSnapshot {
  const cutoff = now - c.windowHours * 3600 * 1000;
  const buf = state.perContractTallies[c.id] ?? [];
  while (buf.length > 0 && buf[0].t < cutoff) buf.shift();
  const totalInWindow = buf.reduce((a, e) => a + e.notional, 0);
  const expired = c.expiresAtISO ? Date.parse(c.expiresAtISO) < now : false;
  return {
    contract: c,
    t: now,
    windowStart: cutoff,
    totalInWindow,
    underFloor: !!c.minNotional && !expired && totalInWindow < c.minNotional,
    overCeiling: !!c.maxNotional && totalInWindow > c.maxNotional,
    expired,
    entries: buf.slice(-8),
  };
}
