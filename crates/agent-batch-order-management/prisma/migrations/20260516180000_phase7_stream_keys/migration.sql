-- AlterTable
ALTER TABLE "settlement_events" ADD COLUMN IF NOT EXISTS "silvana_stream_key" TEXT;

-- CreateIndex
CREATE UNIQUE INDEX IF NOT EXISTS "settlement_events_silvana_stream_key_key" ON "settlement_events"("silvana_stream_key");
