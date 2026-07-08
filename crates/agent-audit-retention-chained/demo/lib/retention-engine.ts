// Port of agent-audit-retention-chained. Generates a multi-day synthetic
// trading-history in memory, splits it into per-day (or per-week)
// slices, and stitches slices via a signed slice header carrying
// prev_slice_hash. Verify walks the slices in chronological order.

export type Slice = Readonly<{
  num: number;
  bucket: string;                  // e.g. "2026-06-25" or "2026-W26"
  recordCount: number;
  prevSliceHash: string;
  sliceHash: string;               // hash of header + all records
  signature: string;
  createdAt: string;
  droppedByRetention: number;
}>;

export type RetentionConfig = Readonly<{
  totalDays: number;               // synthetic history spans this many days
  recordsPerDay: number;
  retentionDays?: number;          // undefined = keep everything
  weekly: boolean;
  party: string;
}>;

export type VerifyReport = Readonly<{
  ok: boolean;
  checked: number;
  brokenAt?: number;
  reason?: string;
}>;

export type RetentionState = {
  config: RetentionConfig;
  slices: Slice[];
  totalRecords: number;
  droppedByRetention: number;
  lastVerify: VerifyReport | null;
  rotations: number;
};

export function initState(config: RetentionConfig): RetentionState {
  return { config, slices: [], totalRecords: 0, droppedByRetention: 0, lastVerify: null, rotations: 0 };
}

export function rotate(state: RetentionState): { state: RetentionState; log: string[] } {
  const log: string[] = [];
  const { totalDays, recordsPerDay, retentionDays, weekly } = state.config;
  const now = Date.now();
  const cutoff = retentionDays !== undefined ? now - retentionDays * 86400 * 1000 : null;

  // Bucket -> list of record hashes (record content simulated by hash only).
  const buckets = new Map<string, string[]>();
  let dropped = 0;
  let total = 0;
  for (let d = 0; d < totalDays; d++) {
    const dayMs = now - (totalDays - 1 - d) * 86400 * 1000;
    for (let r = 0; r < recordsPerDay; r++) {
      const ts = dayMs + r * (86400_000 / Math.max(1, recordsPerDay));
      if (cutoff !== null && ts < cutoff) { dropped += 1; continue; }
      const key = bucketKey(ts, weekly);
      const rec = fauxHash(`rec:${d}:${r}:${ts}`);
      if (!buckets.has(key)) buckets.set(key, []);
      buckets.get(key)!.push(rec);
      total += 1;
    }
  }

  const bucketKeys = [...buckets.keys()].sort();
  const slices: Slice[] = [];
  let prev = "";
  for (const key of bucketKeys) {
    const num = slices.length + 1;
    const records = buckets.get(key)!;
    const created = new Date(now).toISOString();
    const headerLine = `HEAD|${num}|${key}|records=${records.length}|prev=${prev}|created=${created}|party=${state.config.party}`;
    const signature = fauxHash("SIG:" + headerLine);
    // slice hash: header + every record concatenated
    let mixer = headerLine;
    for (const r of records) mixer += "|" + r;
    const sliceHash = fauxHash(mixer);
    slices.push({ num, bucket: key, recordCount: records.length, prevSliceHash: prev, sliceHash, signature, createdAt: created, droppedByRetention: 0 });
    prev = sliceHash;
  }

  state.slices = slices;
  state.totalRecords = total;
  state.droppedByRetention = dropped;
  state.rotations += 1;
  state.lastVerify = null;
  log.push(`ROTATE #${state.rotations}: ${slices.length} slice(s), ${total} records kept, ${dropped} dropped by retention`);
  return { state, log };
}

export function verify(state: RetentionState): { state: RetentionState; log: string[] } {
  const log: string[] = [];
  let expected_prev = "";
  for (let i = 0; i < state.slices.length; i++) {
    const s = state.slices[i];
    if (s.prevSliceHash !== expected_prev) {
      state.lastVerify = { ok: false, checked: i, brokenAt: s.num, reason: "prev_slice_hash mismatch" };
      log.push(`VERIFY FAIL slice #${s.num}: prev mismatch (expected ${expected_prev.slice(0, 8)}…, got ${s.prevSliceHash.slice(0, 8)}…)`);
      return { state, log };
    }
    expected_prev = s.sliceHash;
  }
  state.lastVerify = { ok: true, checked: state.slices.length };
  log.push(`VERIFY OK: ${state.slices.length} slices, chain intact`);
  return { state, log };
}

export function tamperSlice(state: RetentionState, num: number): { state: RetentionState; log: string[] } {
  const idx = state.slices.findIndex((s) => s.num === num);
  if (idx < 0) return { state, log: [`slice #${num} not found`] };
  const s = state.slices[idx];
  // Rehash slice hash with an extra marker — leaves prev_slice_hash of the next slice stale.
  const tampered: Slice = { ...s, sliceHash: fauxHash(s.sliceHash + "|TAMPERED") };
  state.slices = [...state.slices.slice(0, idx), tampered, ...state.slices.slice(idx + 1)];
  return { state, log: [`TAMPER slice #${num}: rehashed — next verify should catch the break`] };
}

function bucketKey(ts: number, weekly: boolean): string {
  const d = new Date(ts);
  if (weekly) {
    // ISO week — YYYY-Www
    const day = d.getUTCDay() || 7;
    const nearestThursday = new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate() - day + 4));
    const yearStart = new Date(Date.UTC(nearestThursday.getUTCFullYear(), 0, 1));
    const weekNum = Math.ceil((((nearestThursday.getTime() - yearStart.getTime()) / 86400_000) + 1) / 7);
    return `${nearestThursday.getUTCFullYear()}-W${weekNum.toString().padStart(2, "0")}`;
  }
  return `${d.getUTCFullYear()}-${(d.getUTCMonth() + 1).toString().padStart(2, "0")}-${d.getUTCDate().toString().padStart(2, "0")}`;
}

function fauxHash(input: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) h = (h ^ input.charCodeAt(i)) * 0x01000193 >>> 0;
  let out = "";
  for (let r = 0; r < 8; r++) { h = (h * 2654435761 + r) >>> 0; out += ("00000000" + h.toString(16)).slice(-8); }
  return out;
}
