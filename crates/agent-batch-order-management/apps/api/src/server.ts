import http from "node:http";
import express from "express";
import { prisma } from "@batch-order/db";
import { analyzeRebalance } from "@batch-order/portfolio-engine";
import { auditRouter } from "./routes/audit.js";
import { executionPreviewRouter } from "./routes/execution.js";
import { healthRouter } from "./routes/health.js";
import { ordersRouter } from "./routes/orders.js";
import { portfolioRouter } from "./routes/portfolio.js";
import { rebalanceRouter } from "./routes/rebalance.js";
import { batchesRouter } from "./routes/batches.js";
import { venuesRouter } from "./routes/venues.js";
import { internalAuthForApi } from "./middleware/internal-auth.js";

const DEV_ENGINE_PORTFOLIO_ID = "00000000-0000-4000-a000-000000000001";

async function bootstrap() {
  const app = express();
  app.use(express.json());

  app.use(internalAuthForApi);

  app.use("/", healthRouter);
  app.use("/api/venues", venuesRouter);
  app.use("/api/execution", executionPreviewRouter);
  app.use("/api/rebalance", rebalanceRouter);
  app.use("/api/batches", batchesRouter);
  app.use("/api/portfolio", portfolioRouter);
  app.use("/api/audit", auditRouter);
  app.use("/api/orders", ordersRouter);

  app.get("/api/internal/bootstrap-check", async (_req, res) => {
    try {
      const tb = Number(process.env.REBALANCE_THRESHOLD_BPS ?? "100");

      const thresholdBps = Number.isFinite(tb) ? tb : 100;
      const engineSample = analyzeRebalance({
        portfolioId: DEV_ENGINE_PORTFOLIO_ID,

        quoteCurrency: "USDC",
        thresholdBps,
        targets: [
          { assetSymbol: "CC", weight: "0.4" },
          { assetSymbol: "USDC", weight: "0.6" },
        ],

        positions: [
          { assetSymbol: "CC", qty: "1000", price: "0.156", marketValue: "156" },
          { assetSymbol: "USDC", qty: "10000", price: "1", marketValue: "10000" },
        ],
      });
      const portfolioEngineDemo = {
        portfolioId: engineSample.portfolioId,
        nav: engineSample.nav,
        estimatedNotional: engineSample.estimatedNotional,
        plannedOrdersPreview: engineSample.plannedOrders.slice(0, 5),
        warnings: engineSample.warnings,
      };

      if (process.env.ENABLE_DB_HEALTH !== "1") {
        res.json({
          portfolioEngineDemo,
          database: null,
          dbHealth: "skipped (set ENABLE_DB_HEALTH=1 и поднимите PostgreSQL)",
        });

        return;
      }

      try {
        const [portfolioCount, targetCount] = await Promise.all([prisma.portfolio.count(), prisma.portfolioTarget.count()]);
        res.json({
          portfolioEngineDemo,
          database: { portfolios: portfolioCount, portfolioTargets: targetCount },

          dbHint: process.env.NODE_ENV !== "production" ? "npm run db:seed" : undefined,
        });
      } catch (err) {
        res.status(503).json({
          portfolioEngineDemo,
          databaseError: err instanceof Error ? err.message : "database unavailable",

        });
      }
    } catch (err) {
      res.status(500).json({
        portfolioEngineError: err instanceof Error ? err.message : "engine_failed",
      });
    }
  });

  const port = Number(process.env.PORT ?? "3000");
  const server = http.createServer(app);
  server.listen(port, () => {
    console.info(`API listening on http://localhost:${port}`);
  });
}

bootstrap().catch((err) => {
  console.error(err);

  process.exit(1);
});
