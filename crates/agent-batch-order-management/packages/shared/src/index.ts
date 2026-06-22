/**
 * Общие типы домена Batch Order Management.
 * Конкретные DTO расширяются при реализации API (фазы 5+).
 */

/** UUID string (camelCase совместимо с PostgreSQL uuid). */
export type EntityId = string;

export type RebalanceJobStatus =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export type OrderBatchStatus =
  | "pending"
  | "submitting"
  | "partially_filled"
  | "filled"
  | "cancelled"
  | "failed";
