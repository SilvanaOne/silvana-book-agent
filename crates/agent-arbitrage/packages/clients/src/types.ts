/**
 * Wire types for the proprietary-engine API.
 *
 * Hand-mirrored from schemas/proprietary-engine-api.yaml (the source of truth).
 * Keep in sync with that file. These are pure request/response shapes — no
 * implementation, safe for open core.
 */

// ───────────────────────────── shared ─────────────────────────────

export type LegStatus = 'pending' | 'success' | 'failed';
export type TradeState =
  | 'queued'
  | 'preflight'
  | 'submitted'
  | 'buy_filled'
  | 'executed'
  | 'partial'
  | 'failed'
  | 'skipped'
  | 'aborted';
export type Side = 'buy' | 'sell';

/** Serializable opportunity sent to the executor (flattened from shared `SpreadOpportunity`). */
export interface SpreadOpportunityDTO {
  readonly baseSymbol: string;
  readonly quoteSymbol: string;
  readonly buyVenue: string;
  readonly sellVenue: string;
  readonly buyPrice: number;
  readonly sellPrice: number;
  readonly spreadPct: number;
  readonly estimatedProfitUsd: number;
  /** epoch ms */
  readonly detectedAt: number;
}

export interface Health {
  readonly status: 'ok' | 'degraded' | 'down';
  readonly [k: string]: unknown;
}

// ──────────────────────────── executor ────────────────────────────

export interface DualTradeRequest {
  readonly opportunity: SpreadOpportunityDTO;
  readonly maxSlippageBps: number;
  readonly safetyBufferBps: number;
  readonly dryRun: boolean;
  readonly idempotencyKey: string;
}

export interface DualTradeAccepted {
  readonly tradeId: string;
  readonly status: 'queued';
}

export interface LegDetail {
  readonly venue: string;
  readonly side: Side;
  readonly status: LegStatus;
  readonly orderId?: string;
  readonly clientOrderId?: string;
  readonly qty?: number;
  readonly price?: number;
  readonly usdt?: number;
  readonly fee?: number;
  readonly error?: string;
}

export interface DualTradeStatus {
  readonly tradeId: string;
  readonly status: TradeState;
  readonly partialExecution: boolean;
  readonly legBuy?: LegDetail;
  readonly legSell?: LegDetail;
  readonly detected?: { buyPrice?: number; sellPrice?: number; spreadPct?: number };
  readonly actual?: { buyPrice?: number; sellPrice?: number; buySlippagePct?: number; sellSlippagePct?: number };
  readonly profit?: { usdtDiff?: number; tokenPct?: number };
  readonly timing?: { buyExecutedAt?: number; sellExecutedAt?: number; totalExecutionMs?: number };
  readonly failReason?: string;
}

export interface AbortResult {
  readonly aborted: boolean;
  readonly cleanupActions?: readonly string[];
}

export interface EmergencyStopResult {
  readonly stopped: boolean;
  readonly inflightAborted: readonly string[];
}

// ─────────────────────────── rebalancer ───────────────────────────

export interface RebalanceRequest {
  readonly fromVenue: string;
  readonly toVenue: string;
  readonly asset: string;
  /** decimal string */
  readonly amountNative: string;
  readonly maxRouteCostBps?: number;
  readonly deadlineUnix?: number;
  readonly idempotencyKey: string;
}

export interface RebalanceAccepted {
  readonly jobId: string;
  readonly estimatedArrivalTs?: number;
  readonly estimatedCostBps?: number;
  readonly routeSummary?: string;
}

export interface RebalanceLeg {
  readonly kind?: string;
  readonly venue?: string;
  readonly asset?: string;
  readonly amountNative?: string;
  readonly status?: string;
  readonly txRef?: string;
}

export interface RebalanceJob {
  readonly jobId: string;
  readonly status: 'queued' | 'inflight' | 'completed' | 'failed';
  readonly legs?: readonly RebalanceLeg[];
}

export interface InventorySnapshot {
  readonly venue: string;
  readonly asset: string;
  readonly free: string;
  readonly locked: string;
  readonly lastUpdatedTs?: number;
}

export interface InventoryResponse {
  readonly snapshots: readonly InventorySnapshot[];
}

export interface SkewEntry {
  readonly venue: string;
  readonly asset: string;
  readonly currentUsd?: number;
  readonly targetUsd?: number;
  readonly skewPct: number;
  readonly actionHint?: string;
}

export interface SkewResponse {
  readonly skews: readonly SkewEntry[];
}

export interface HedgeRequest {
  readonly side: 'short' | 'long';
  readonly venue: string;
  readonly asset: string;
  readonly notionalUsd: number;
}

export interface HedgeResult {
  readonly hedgeId: string;
  readonly positionSize?: number;
  readonly entryPrice?: number;
}

export interface CloseHedgeResult {
  readonly closed: boolean;
  readonly realizedPnlUsd?: number;
}

// ───────────────────────────── signer ─────────────────────────────

export interface BalanceSource {
  readonly id: string;
  readonly label: string;
  readonly kind: 'cex' | 'canton';
  readonly totalUsd: number;
  readonly error?: string;
}

export interface BalanceRow {
  readonly sourceId: string;
  readonly sourceLabel: string;
  readonly kind: 'cex' | 'canton';
  readonly venueOrChain: string;
  readonly asset: string;
  readonly free: number;
  readonly locked: number;
  readonly usd: number | null;
  readonly address?: string;
}

export interface BalanceSnapshot {
  readonly updatedAt: number;
  readonly totalUsd: number;
  readonly sources: readonly BalanceSource[];
  readonly rows: readonly BalanceRow[];
}

export interface PublicKeyResponse {
  readonly pem: string;
  readonly keyId: string;
}

export interface SaveCexSecretRequest {
  readonly venue: string;
  readonly keyId: string;
  readonly apiKeyCiphertext: string;
  readonly secretCiphertext: string;
  readonly passphraseCiphertext?: string;
}

export interface SaveWalletSecretRequest {
  readonly label: string;
  readonly chain: string;
  readonly keyId: string;
  readonly privKeyCiphertext?: string;
  readonly seedCiphertext?: string;
}

export interface SecretSlot {
  readonly id: string;
  readonly kind: 'cex' | 'wallet';
  readonly filled: boolean;
  readonly venue?: string;
  readonly label?: string;
  readonly rsaKeyId?: string;
}

export interface SecretsStatusResponse {
  readonly slots: readonly SecretSlot[];
}

export type EngineName = 'executor' | 'rebalancer';

export interface RuntimeBundleRequest {
  readonly engine: EngineName;
}

export interface DecryptedSecret {
  readonly id: string;
  readonly kind: 'cex' | 'wallet';
  readonly venue?: string;
  readonly label?: string;
  readonly apiKey?: string;
  readonly apiSecret?: string;
  readonly passphrase?: string;
  readonly privKey?: string;
}

export interface RuntimeBundleResponse {
  readonly engine: EngineName;
  readonly secrets: readonly DecryptedSecret[];
}
