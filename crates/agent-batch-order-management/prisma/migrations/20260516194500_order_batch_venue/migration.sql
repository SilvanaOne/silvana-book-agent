-- AlterTable
ALTER TABLE "order_batches" ADD COLUMN "venue" TEXT NOT NULL DEFAULT 'silvana';

-- CreateIndex
CREATE INDEX "order_batches_rebalance_job_id_venue_market_idx" ON "order_batches"("rebalance_job_id", "venue", "market");
