import { BaseEngineClient } from './base.js';
import type {
  CloseHedgeResult,
  Health,
  HedgeRequest,
  HedgeResult,
  InventoryResponse,
  RebalanceAccepted,
  RebalanceJob,
  RebalanceRequest,
  SkewResponse,
} from './types.js';

/**
 * Typed client for the proprietary REBALANCER engine (cross-venue inventory
 * routing + hedge). Open core calls this; the engine is a separate private repo.
 */
export class RebalancerClient extends BaseEngineClient {
  protected readonly engineName = 'rebalancer';

  /** Request a cross-venue inventory move. Returns a jobId to poll. */
  requestRebalance(req: RebalanceRequest): Promise<RebalanceAccepted> {
    return this.request<RebalanceAccepted>('POST', '/rebalance', req);
  }

  /** Poll a rebalance job. */
  getRebalanceJob(jobId: string): Promise<RebalanceJob> {
    return this.request<RebalanceJob>('GET', `/rebalance/${encodeURIComponent(jobId)}`);
  }

  /** Current inventory snapshots per venue/asset. */
  getInventory(): Promise<InventoryResponse> {
    return this.request<InventoryResponse>('GET', '/inventory');
  }

  /** Inventory skew per venue/asset vs target. */
  getSkew(): Promise<SkewResponse> {
    return this.request<SkewResponse>('GET', '/skew');
  }

  /** Open a perp hedge position. */
  openHedge(req: HedgeRequest): Promise<HedgeResult> {
    return this.request<HedgeResult>('POST', '/hedge', req);
  }

  /** Close a perp hedge position. */
  closeHedge(hedgeId: string): Promise<CloseHedgeResult> {
    return this.request<CloseHedgeResult>('POST', `/hedge/close/${encodeURIComponent(hedgeId)}`);
  }

  health(): Promise<Health> {
    return this.request<Health>('GET', '/health');
  }
}
