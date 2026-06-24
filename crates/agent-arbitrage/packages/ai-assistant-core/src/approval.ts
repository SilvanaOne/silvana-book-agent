/**
 * ApprovalManager — the "AI suggests, operator confirms" gate (md_docs/13).
 *
 * Any AI action that would change state (e.g. an NL-config diff) is parked here
 * as a pending approval with a TTL. Nothing is applied until the operator
 * confirms; the manager only records the decision — applying the diff is the
 * operator-UI's job (via the open-core /api/config path), never the AI service.
 */

export interface PendingApproval<T = unknown> {
  readonly id: string;
  readonly kind: string;
  readonly payload: T;
  readonly createdAt: number;
  readonly expiresAt: number;
  status: 'pending' | 'confirmed' | 'rejected' | 'expired';
}

export class ApprovalManager {
  private readonly items = new Map<string, PendingApproval>();
  private seq = 0;

  constructor(
    private readonly ttlMs = 5 * 60_000,
    private readonly now: () => number = Date.now,
  ) {}

  create<T>(kind: string, payload: T): PendingApproval<T> {
    this.seq += 1;
    const t = this.now();
    const item: PendingApproval<T> = {
      id: `appr-${this.seq}`,
      kind,
      payload,
      createdAt: t,
      expiresAt: t + this.ttlMs,
      status: 'pending',
    };
    this.items.set(item.id, item as PendingApproval);
    return item;
  }

  private resolveExpiry(item: PendingApproval): PendingApproval {
    if (item.status === 'pending' && this.now() > item.expiresAt) item.status = 'expired';
    return item;
  }

  get(id: string): PendingApproval | undefined {
    const item = this.items.get(id);
    return item ? this.resolveExpiry(item) : undefined;
  }

  /** Confirm a pending approval; returns its payload, or null if not confirmable. */
  confirm(id: string): unknown | null {
    const item = this.get(id);
    if (!item || item.status !== 'pending') return null;
    item.status = 'confirmed';
    return item.payload;
  }

  reject(id: string): boolean {
    const item = this.get(id);
    if (!item || item.status !== 'pending') return false;
    item.status = 'rejected';
    return true;
  }
}
