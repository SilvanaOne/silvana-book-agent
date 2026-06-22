-- CreateSchema
CREATE SCHEMA IF NOT EXISTS "public";

-- CreateTable
CREATE TABLE "portfolios" (
    "id" UUID NOT NULL,
    "name" TEXT NOT NULL,
    "base_currency" TEXT NOT NULL,
    "created_at" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_at" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "portfolios_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "portfolio_targets" (
    "id" UUID NOT NULL,
    "portfolio_id" UUID NOT NULL,
    "asset_symbol" TEXT NOT NULL,
    "target_weight" DECIMAL(18,8) NOT NULL,
    "min_weight" DECIMAL(18,8),
    "max_weight" DECIMAL(18,8),
    "enabled" BOOLEAN NOT NULL,
    "created_at" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_at" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "portfolio_targets_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "position_snapshots" (
    "id" UUID NOT NULL,
    "portfolio_id" UUID NOT NULL,
    "asset_symbol" TEXT NOT NULL,
    "qty" DECIMAL(30,12) NOT NULL,
    "market_value" DECIMAL(30,12) NOT NULL,
    "price" DECIMAL(30,12) NOT NULL,
    "as_of" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "position_snapshots_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "rebalance_jobs" (
    "id" UUID NOT NULL,
    "portfolio_id" UUID NOT NULL,
    "status" TEXT NOT NULL,
    "mode" TEXT NOT NULL,
    "requested_by" TEXT,
    "dry_run" BOOLEAN NOT NULL DEFAULT false,
    "input" JSONB NOT NULL,
    "output" JSONB,
    "started_at" TIMESTAMP(3),
    "finished_at" TIMESTAMP(3),
    "created_at" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "rebalance_jobs_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "order_batches" (
    "id" UUID NOT NULL,
    "rebalance_job_id" UUID NOT NULL,
    "status" TEXT NOT NULL,
    "market" TEXT NOT NULL,
    "total_orders" INTEGER NOT NULL,
    "submitted_orders" INTEGER NOT NULL DEFAULT 0,
    "filled_orders" INTEGER NOT NULL DEFAULT 0,
    "cancelled_orders" INTEGER NOT NULL DEFAULT 0,
    "created_at" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_at" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "order_batches_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "orders" (
    "id" UUID NOT NULL,
    "order_batch_id" UUID NOT NULL,
    "silvana_order_id" TEXT,
    "side" TEXT NOT NULL,
    "type" TEXT NOT NULL,
    "market" TEXT NOT NULL,
    "price" DECIMAL(30,12),
    "qty" DECIMAL(30,12) NOT NULL,
    "status" TEXT NOT NULL,
    "client_order_ref" TEXT,
    "created_at" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_at" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "orders_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "order_events" (
    "id" UUID NOT NULL,
    "order_id" UUID NOT NULL,
    "event_type" TEXT NOT NULL,
    "payload" JSONB NOT NULL,
    "silvana_event_key" TEXT,
    "created_at" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "order_events_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "settlement_events" (
    "id" UUID NOT NULL,
    "rebalance_job_id" UUID NOT NULL,
    "payload" JSONB NOT NULL,
    "status" TEXT,
    "created_at" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "settlement_events_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "audit_logs" (
    "id" UUID NOT NULL,
    "actor_id" TEXT NOT NULL,
    "action" TEXT NOT NULL,
    "entity_type" TEXT NOT NULL,
    "entity_id" TEXT NOT NULL,
    "payload" JSONB NOT NULL,
    "created_at" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "audit_logs_pkey" PRIMARY KEY ("id")
);

-- CreateIndex
CREATE INDEX "portfolio_targets_portfolio_id_idx" ON "portfolio_targets"("portfolio_id");

-- CreateIndex
CREATE UNIQUE INDEX "portfolio_targets_portfolio_id_asset_symbol_key" ON "portfolio_targets"("portfolio_id", "asset_symbol");

-- CreateIndex
CREATE INDEX "position_snapshots_portfolio_id_as_of_idx" ON "position_snapshots"("portfolio_id", "as_of");

-- CreateIndex
CREATE INDEX "rebalance_jobs_portfolio_id_status_idx" ON "rebalance_jobs"("portfolio_id", "status");

-- CreateIndex
CREATE INDEX "rebalance_jobs_status_created_at_idx" ON "rebalance_jobs"("status", "created_at");

-- CreateIndex
CREATE INDEX "order_batches_rebalance_job_id_idx" ON "order_batches"("rebalance_job_id");

-- CreateIndex
CREATE INDEX "order_batches_status_idx" ON "order_batches"("status");

-- CreateIndex
CREATE INDEX "orders_client_order_ref_idx" ON "orders"("client_order_ref");

-- CreateIndex
CREATE INDEX "orders_order_batch_id_idx" ON "orders"("order_batch_id");

-- CreateIndex
CREATE UNIQUE INDEX "orders_silvana_order_id_key" ON "orders"("silvana_order_id");

-- CreateIndex
CREATE UNIQUE INDEX "order_events_silvana_event_key_key" ON "order_events"("silvana_event_key");

-- CreateIndex
CREATE INDEX "order_events_order_id_idx" ON "order_events"("order_id");

-- CreateIndex
CREATE INDEX "order_events_event_type_idx" ON "order_events"("event_type");

-- CreateIndex
CREATE INDEX "settlement_events_rebalance_job_id_idx" ON "settlement_events"("rebalance_job_id");

-- CreateIndex
CREATE INDEX "audit_logs_actor_id_idx" ON "audit_logs"("actor_id");

-- CreateIndex
CREATE INDEX "audit_logs_entity_type_entity_id_idx" ON "audit_logs"("entity_type", "entity_id");

-- CreateIndex
CREATE INDEX "audit_logs_created_at_idx" ON "audit_logs"("created_at");

-- AddForeignKey
ALTER TABLE "portfolio_targets" ADD CONSTRAINT "portfolio_targets_portfolio_id_fkey" FOREIGN KEY ("portfolio_id") REFERENCES "portfolios"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "position_snapshots" ADD CONSTRAINT "position_snapshots_portfolio_id_fkey" FOREIGN KEY ("portfolio_id") REFERENCES "portfolios"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "rebalance_jobs" ADD CONSTRAINT "rebalance_jobs_portfolio_id_fkey" FOREIGN KEY ("portfolio_id") REFERENCES "portfolios"("id") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "order_batches" ADD CONSTRAINT "order_batches_rebalance_job_id_fkey" FOREIGN KEY ("rebalance_job_id") REFERENCES "rebalance_jobs"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "orders" ADD CONSTRAINT "orders_order_batch_id_fkey" FOREIGN KEY ("order_batch_id") REFERENCES "order_batches"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "order_events" ADD CONSTRAINT "order_events_order_id_fkey" FOREIGN KEY ("order_id") REFERENCES "orders"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "settlement_events" ADD CONSTRAINT "settlement_events_rebalance_job_id_fkey" FOREIGN KEY ("rebalance_job_id") REFERENCES "rebalance_jobs"("id") ON DELETE CASCADE ON UPDATE CASCADE;

