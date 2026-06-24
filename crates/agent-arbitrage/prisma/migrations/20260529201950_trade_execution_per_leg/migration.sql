-- CreateTable
CREATE TABLE "TradeExecution" (
    "id" BIGSERIAL NOT NULL,
    "executorTradeId" TEXT,
    "opportunityId" TEXT,
    "ts" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "ticker" TEXT NOT NULL,
    "buyVenueId" TEXT NOT NULL,
    "sellVenueId" TEXT NOT NULL,
    "status" TEXT NOT NULL,
    "partialExecution" BOOLEAN NOT NULL DEFAULT false,
    "detectedBuyPrice" DECIMAL(36,18),
    "detectedSellPrice" DECIMAL(36,18),
    "spreadBps" INTEGER,
    "legBuyStatus" TEXT,
    "legBuyOrderId" TEXT,
    "legBuyQty" DECIMAL(36,18),
    "legBuyUsdt" DECIMAL(36,18),
    "legBuyError" TEXT,
    "legSellStatus" TEXT,
    "legSellOrderId" TEXT,
    "legSellQty" DECIMAL(36,18),
    "legSellUsdt" DECIMAL(36,18),
    "legSellError" TEXT,
    "actualBuyPrice" DECIMAL(36,18),
    "actualSellPrice" DECIMAL(36,18),
    "buySlippageBps" INTEGER,
    "sellSlippageBps" INTEGER,
    "profitUsdtDiff" DECIMAL(36,18),
    "profitTokenPct" DECIMAL(36,18),
    "buyExecutedAt" TIMESTAMP(3),
    "sellExecutedAt" TIMESTAMP(3),
    "totalExecMs" INTEGER,
    "failReason" TEXT,

    CONSTRAINT "TradeExecution_pkey" PRIMARY KEY ("id")
);

-- CreateIndex
CREATE UNIQUE INDEX "TradeExecution_executorTradeId_key" ON "TradeExecution"("executorTradeId");

-- CreateIndex
CREATE INDEX "TradeExecution_ts_idx" ON "TradeExecution"("ts");

-- CreateIndex
CREATE INDEX "TradeExecution_status_idx" ON "TradeExecution"("status");

-- CreateIndex
CREATE INDEX "TradeExecution_opportunityId_idx" ON "TradeExecution"("opportunityId");
