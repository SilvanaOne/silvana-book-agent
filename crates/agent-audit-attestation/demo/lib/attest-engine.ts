// Port of agent-audit-attestation. Reads the head of a simulated
// trading-history JSONL, signs a checkpoint carrying head_seq +
// head_hash, and appends it to an in-memory checkpoint log.
//
// The demo simulates a growing trading-history log: every tick it
// appends a fresh record; on the configured schedule it publishes a
// checkpoint of the current head. Auditors could later use any single
// checkpoint to prove the earlier records existed at that time.

export type HistoryRecord = Readonly<{
  seq: number;
  ts: string;
  prev_hash: string;
  kind: string;
  payload_sha256: string;
  line_hash: string;
}>;

export type Checkpoint = Readonly<{
  seq: number;                     // checkpoint sequence
  publishedAt: number;
  ts: string;
  headSeq: number;
  headHash: string;
  records: number;
  party: string;
  signature: string;
}>;

export type AttestConfig = Readonly<{
  party: string;
  historyGrowthPerSec: number;     // records appended per second
  intervalSecs: number;            // publish cadence; 0 = manual only
  webhookFailureRate: number;      // 0..1 — mimic sink failure
}>;

export type AttestState = {
  config: AttestConfig;
  status: "running" | "idle";
  history: HistoryRecord[];        // bounded ~150
  checkpoints: Checkpoint[];       // bounded ~30
  nextRecordSeq: number;
  nextCheckpointSeq: number;
  nextRecordAt: number;
  nextCheckpointAt: number | null;
  totalPublished: number;
  totalFailed: number;
};

const MAX_HISTORY = 300;
const MAX_CHECKPOINTS = 30;
const KINDS = ["order.created", "order.filled", "order.cancelled", "settlement.settled", "settlement.failed"] as const;

export function initState(config: AttestConfig, now: number): AttestState {
  return {
    config,
    status: "running",
    history: [],
    checkpoints: [],
    nextRecordSeq: 1,
    nextCheckpointSeq: 1,
    nextRecordAt: now,
    nextCheckpointAt: config.intervalSecs > 0 ? now + config.intervalSecs * 1000 : null,
    totalPublished: 0,
    totalFailed: 0,
  };
}

export function step(state: AttestState, _price: number, now: number): { state: AttestState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];
  const recIntervalMs = 1000 / Math.max(0.1, state.config.historyGrowthPerSec);
  while (now >= state.nextRecordAt) {
    appendRecord(state, state.nextRecordAt);
    state.nextRecordAt += recIntervalMs;
  }
  if (state.nextCheckpointAt !== null && now >= state.nextCheckpointAt) {
    const cp = publish(state, now, log);
    if (cp) state.nextCheckpointAt = now + state.config.intervalSecs * 1000;
  }
  return { state, log };
}

export function publishManual(state: AttestState, now: number): { state: AttestState; log: string[] } {
  const log: string[] = [];
  publish(state, now, log);
  return { state, log };
}

function appendRecord(state: AttestState, t: number): void {
  const seq = state.nextRecordSeq++;
  const prev_hash = state.history.length > 0 ? state.history[state.history.length - 1].line_hash : "";
  const kind = KINDS[Math.floor(Math.random() * KINDS.length)];
  const payload_sha256 = fauxHash(`${seq}:${kind}:${Math.random()}`);
  const ts = new Date(t).toISOString();
  const canonical = `${seq}|${ts}|${prev_hash}|${kind}|${payload_sha256}`;
  const line_hash = fauxHash(canonical + "|record");
  state.history.push({ seq, ts, prev_hash, kind, payload_sha256, line_hash });
  while (state.history.length > MAX_HISTORY) state.history.shift();
}

function publish(state: AttestState, now: number, log: string[]): Checkpoint | null {
  if (state.history.length === 0) {
    log.push("SKIP  history is empty — nothing to attest");
    return null;
  }
  const head = state.history[state.history.length - 1];
  const seq = state.nextCheckpointSeq++;
  const ts = new Date(now).toISOString();
  const payload = `checkpoint|${seq}|${ts}|${state.config.party}|${head.seq}|${head.line_hash}`;
  const signature = fauxHash("SIG:" + payload);
  const failed = Math.random() < state.config.webhookFailureRate;
  if (failed) {
    state.totalFailed += 1;
    log.push(`FAIL  checkpoint #${seq} head_seq=${head.seq} — webhook 5xx`);
    return null;
  }
  const cp: Checkpoint = { seq, publishedAt: now, ts, headSeq: head.seq, headHash: head.line_hash, records: state.history.length, party: state.config.party, signature };
  state.checkpoints.push(cp);
  while (state.checkpoints.length > MAX_CHECKPOINTS) state.checkpoints.shift();
  state.totalPublished += 1;
  log.push(`PUB   checkpoint #${seq} head_seq=${head.seq} hash=${head.line_hash.slice(0, 12)}…`);
  return cp;
}

function fauxHash(input: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) h = (h ^ input.charCodeAt(i)) * 0x01000193 >>> 0;
  let out = "";
  for (let r = 0; r < 8; r++) {
    h = (h * 2654435761 + r) >>> 0;
    out += ("00000000" + h.toString(16)).slice(-8);
  }
  return out;
}
