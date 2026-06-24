/**
 * Cost + rate guard (md_docs/13 cross-cutting safety). Per-operator daily query
 * cap and a per-minute rate limit. Skeleton: in-memory counters (real deployment
 * tracks cost in an AiQuery table). Heuristic provider is free, but the guard
 * still rate-limits to protect against runaway loops.
 */

export interface GuardDecision {
  readonly ok: boolean;
  readonly reason?: string;
}

export class CostGuard {
  private readonly perDay = new Map<string, { day: number; count: number }>();
  private readonly recent = new Map<string, number[]>();

  constructor(
    private readonly maxPerDay = 500,
    private readonly maxPerMinute = 30,
    private readonly now: () => number = Date.now,
  ) {}

  check(operatorId: string): GuardDecision {
    const t = this.now();
    const day = Math.floor(t / 86_400_000);

    const d = this.perDay.get(operatorId);
    if (!d || d.day !== day) {
      this.perDay.set(operatorId, { day, count: 1 });
    } else if (d.count >= this.maxPerDay) {
      return { ok: false, reason: 'daily AI query cap reached' };
    } else {
      d.count += 1;
    }

    const windowStart = t - 60_000;
    const hits = (this.recent.get(operatorId) ?? []).filter((ts) => ts > windowStart);
    if (hits.length >= this.maxPerMinute) {
      return { ok: false, reason: 'rate limit: too many AI queries per minute' };
    }
    hits.push(t);
    this.recent.set(operatorId, hits);
    return { ok: true };
  }
}
