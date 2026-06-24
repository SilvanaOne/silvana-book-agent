-- CreateTable
CREATE TABLE "Venue" (
    "id" TEXT NOT NULL,
    "type" TEXT NOT NULL,
    "displayName" TEXT NOT NULL,
    "enabled" BOOLEAN NOT NULL DEFAULT true,
    "config" JSONB NOT NULL DEFAULT '{}',
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "Venue_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "Instrument" (
    "id" TEXT NOT NULL,
    "symbol" TEXT NOT NULL,
    "decimals" INTEGER NOT NULL,
    "chainId" TEXT NOT NULL,
    "metadata" JSONB NOT NULL DEFAULT '{}',
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "Instrument_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "Market" (
    "id" TEXT NOT NULL,
    "venueId" TEXT NOT NULL,
    "baseId" TEXT NOT NULL,
    "quoteId" TEXT NOT NULL,
    "enabled" BOOLEAN NOT NULL DEFAULT true,
    "metadata" JSONB NOT NULL DEFAULT '{}',
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "Market_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "PriceTick" (
    "id" BIGSERIAL NOT NULL,
    "venueId" TEXT NOT NULL,
    "marketId" TEXT NOT NULL,
    "ts" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "bid" DECIMAL(36,18) NOT NULL,
    "ask" DECIMAL(36,18) NOT NULL,
    "mid" DECIMAL(36,18) NOT NULL,
    "metadata" JSONB NOT NULL DEFAULT '{}',

    CONSTRAINT "PriceTick_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "Spread" (
    "id" BIGSERIAL NOT NULL,
    "ts" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "basePairKey" TEXT NOT NULL,
    "buyVenueId" TEXT NOT NULL,
    "sellVenueId" TEXT NOT NULL,
    "buyPrice" DECIMAL(36,18) NOT NULL,
    "sellPrice" DECIMAL(36,18) NOT NULL,
    "spreadBps" INTEGER NOT NULL,
    "estProfitUsd" DECIMAL(36,18) NOT NULL,
    "acted" BOOLEAN NOT NULL DEFAULT false,
    "opportunityId" TEXT,

    CONSTRAINT "Spread_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "Opportunity" (
    "id" TEXT NOT NULL,
    "detectedAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "basePairKey" TEXT NOT NULL,
    "buyVenueId" TEXT NOT NULL,
    "sellVenueId" TEXT NOT NULL,
    "spreadBps" INTEGER NOT NULL,
    "estProfitUsd" DECIMAL(36,18) NOT NULL,
    "status" TEXT NOT NULL DEFAULT 'queued',
    "executorTradeId" TEXT,
    "realizedProfitUsd" DECIMAL(36,18),
    "errorMessage" TEXT,

    CONSTRAINT "Opportunity_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "TradeAudit" (
    "id" BIGSERIAL NOT NULL,
    "opportunityId" TEXT,
    "executorTradeId" TEXT,
    "ts" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "eventType" TEXT NOT NULL,
    "legId" TEXT,
    "venueId" TEXT,
    "payload" JSONB NOT NULL DEFAULT '{}',

    CONSTRAINT "TradeAudit_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "RebalanceAudit" (
    "id" BIGSERIAL NOT NULL,
    "jobId" TEXT,
    "ts" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "fromVenueId" TEXT,
    "toVenueId" TEXT,
    "asset" TEXT NOT NULL,
    "amountNative" DECIMAL(36,18),
    "status" TEXT NOT NULL,
    "payload" JSONB NOT NULL DEFAULT '{}',

    CONSTRAINT "RebalanceAudit_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "InventorySnapshot" (
    "id" BIGSERIAL NOT NULL,
    "venueId" TEXT NOT NULL,
    "asset" TEXT NOT NULL,
    "free" DECIMAL(36,18) NOT NULL,
    "locked" DECIMAL(36,18) NOT NULL,
    "ts" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "InventorySnapshot_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "Config" (
    "key" TEXT NOT NULL,
    "value" JSONB NOT NULL,
    "updatedAt" TIMESTAMP(3) NOT NULL,
    "updatedBy" TEXT,

    CONSTRAINT "Config_pkey" PRIMARY KEY ("key")
);

-- CreateTable
CREATE TABLE "AuditLog" (
    "id" BIGSERIAL NOT NULL,
    "ts" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "actor" TEXT NOT NULL,
    "action" TEXT NOT NULL,
    "payload" JSONB NOT NULL DEFAULT '{}',

    CONSTRAINT "AuditLog_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "Operator" (
    "id" TEXT NOT NULL,
    "username" TEXT NOT NULL,
    "passwordHash" TEXT NOT NULL,
    "enabled" BOOLEAN NOT NULL DEFAULT true,
    "preferredLanguage" TEXT NOT NULL DEFAULT 'en',
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "Operator_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "RsaKeypair" (
    "id" SERIAL NOT NULL,
    "publicKeyPem" TEXT NOT NULL,
    "privateKeyPem" TEXT NOT NULL,
    "purpose" TEXT NOT NULL,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "RsaKeypair_pkey" PRIMARY KEY ("id")
);

-- CreateIndex
CREATE INDEX "Market_venueId_idx" ON "Market"("venueId");

-- CreateIndex
CREATE UNIQUE INDEX "Market_venueId_baseId_quoteId_key" ON "Market"("venueId", "baseId", "quoteId");

-- CreateIndex
CREATE INDEX "PriceTick_marketId_ts_idx" ON "PriceTick"("marketId", "ts");

-- CreateIndex
CREATE INDEX "PriceTick_ts_idx" ON "PriceTick"("ts");

-- CreateIndex
CREATE INDEX "Spread_basePairKey_ts_idx" ON "Spread"("basePairKey", "ts");

-- CreateIndex
CREATE INDEX "Spread_ts_idx" ON "Spread"("ts");

-- CreateIndex
CREATE UNIQUE INDEX "Opportunity_executorTradeId_key" ON "Opportunity"("executorTradeId");

-- CreateIndex
CREATE INDEX "Opportunity_detectedAt_idx" ON "Opportunity"("detectedAt");

-- CreateIndex
CREATE INDEX "Opportunity_status_idx" ON "Opportunity"("status");

-- CreateIndex
CREATE INDEX "TradeAudit_opportunityId_ts_idx" ON "TradeAudit"("opportunityId", "ts");

-- CreateIndex
CREATE INDEX "TradeAudit_ts_idx" ON "TradeAudit"("ts");

-- CreateIndex
CREATE INDEX "RebalanceAudit_jobId_idx" ON "RebalanceAudit"("jobId");

-- CreateIndex
CREATE INDEX "RebalanceAudit_ts_idx" ON "RebalanceAudit"("ts");

-- CreateIndex
CREATE INDEX "InventorySnapshot_venueId_asset_ts_idx" ON "InventorySnapshot"("venueId", "asset", "ts");

-- CreateIndex
CREATE INDEX "InventorySnapshot_ts_idx" ON "InventorySnapshot"("ts");

-- CreateIndex
CREATE INDEX "AuditLog_ts_idx" ON "AuditLog"("ts");

-- CreateIndex
CREATE INDEX "AuditLog_actor_ts_idx" ON "AuditLog"("actor", "ts");

-- CreateIndex
CREATE UNIQUE INDEX "Operator_username_key" ON "Operator"("username");

-- AddForeignKey
ALTER TABLE "Market" ADD CONSTRAINT "Market_venueId_fkey" FOREIGN KEY ("venueId") REFERENCES "Venue"("id") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "Market" ADD CONSTRAINT "Market_baseId_fkey" FOREIGN KEY ("baseId") REFERENCES "Instrument"("id") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "Market" ADD CONSTRAINT "Market_quoteId_fkey" FOREIGN KEY ("quoteId") REFERENCES "Instrument"("id") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "PriceTick" ADD CONSTRAINT "PriceTick_venueId_fkey" FOREIGN KEY ("venueId") REFERENCES "Venue"("id") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "PriceTick" ADD CONSTRAINT "PriceTick_marketId_fkey" FOREIGN KEY ("marketId") REFERENCES "Market"("id") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "Spread" ADD CONSTRAINT "Spread_buyVenueId_fkey" FOREIGN KEY ("buyVenueId") REFERENCES "Venue"("id") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "Spread" ADD CONSTRAINT "Spread_sellVenueId_fkey" FOREIGN KEY ("sellVenueId") REFERENCES "Venue"("id") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "Spread" ADD CONSTRAINT "Spread_opportunityId_fkey" FOREIGN KEY ("opportunityId") REFERENCES "Opportunity"("id") ON DELETE SET NULL ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "TradeAudit" ADD CONSTRAINT "TradeAudit_opportunityId_fkey" FOREIGN KEY ("opportunityId") REFERENCES "Opportunity"("id") ON DELETE SET NULL ON UPDATE CASCADE;
