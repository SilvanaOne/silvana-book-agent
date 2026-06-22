-- AlterTable
ALTER TABLE "orders" ADD COLUMN "venue" TEXT NOT NULL DEFAULT 'silvana';
ALTER TABLE "orders" ADD COLUMN "venue_order_ref" TEXT;
ALTER TABLE "orders" ADD COLUMN "exec_profile" TEXT;

-- Backfill внешнего референса для уже отправленных в Silvana ордеров
UPDATE "orders" SET "venue_order_ref" = "silvana_order_id" WHERE "silvana_order_id" IS NOT NULL AND "venue" = 'silvana';

CREATE UNIQUE INDEX "orders_venue_venue_order_ref_key" ON "orders"("venue", "venue_order_ref");
