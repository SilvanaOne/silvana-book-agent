import { PrismaClient } from "@prisma/client";
import { Decimal } from "@prisma/client/runtime/library";

const prisma = new PrismaClient();

/** Stable UUID for the local dev portfolio. Re-running seed upserts/updates the same record. */
const DEV_PORTFOLIO_ID = "00000000-0000-4000-a000-000000000001";

async function seed() {
  await prisma.portfolio.upsert({
    where: { id: DEV_PORTFOLIO_ID },
    create: {
      id: DEV_PORTFOLIO_ID,
      name: "Development portfolio",
      baseCurrency: "USDC",
    },
    update: {
      name: "Development portfolio",
      baseCurrency: "USDC",
    },
  });

  await prisma.portfolioTarget.deleteMany({ where: { portfolioId: DEV_PORTFOLIO_ID } });

  // Four assets: two majors (WBTC, WETH), one minor (CC), and a quote-currency buffer (USDC).
  // Sum of enabled weights = 1.0.
  await prisma.portfolioTarget.createMany({
    data: [
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "WBTC",
        targetWeight: new Decimal("0.25"),
        minWeight: null,
        maxWeight: null,
        enabled: true,
      },
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "WETH",
        targetWeight: new Decimal("0.25"),
        minWeight: null,
        maxWeight: null,
        enabled: true,
      },
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "CC",
        targetWeight: new Decimal("0.1"),
        minWeight: null,
        maxWeight: null,
        enabled: true,
      },
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "USDC",
        targetWeight: new Decimal("0.4"),
        minWeight: new Decimal("0.1"),
        maxWeight: new Decimal("0.9"),
        enabled: true,
      },
    ],
  });

  await prisma.positionSnapshot.deleteMany({ where: { portfolioId: DEV_PORTFOLIO_ID } });

  // Initial snapshot. Prices are demo-only placeholders; in production they come from the pricing stream.
  // Total NAV ≈ 20406 USDC, drift vs targets is intentionally non-trivial so the dashboard is interesting on first load.
  const asOf = new Date();
  await prisma.positionSnapshot.createMany({
    data: [
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "WBTC",
        qty: new Decimal("0.05"),
        marketValue: new Decimal("5000"),
        price: new Decimal("100000"),
        asOf,
      },
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "WETH",
        qty: new Decimal("1.5"),
        marketValue: new Decimal("5250"),
        price: new Decimal("3500"),
        asOf,
      },
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "CC",
        qty: new Decimal("1000"),
        marketValue: new Decimal("156"),
        price: new Decimal("0.156"),
        asOf,
      },
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "USDC",
        qty: new Decimal("10000"),
        marketValue: new Decimal("10000"),
        price: new Decimal("1"),
        asOf,
      },
    ],
  });

  console.info("[seed] development portfolio/targets/snapshot ready:", DEV_PORTFOLIO_ID);
}

seed()
  .catch((err) => {
    console.error(err);
    process.exit(1);
  })
  .finally(async () => {
    await prisma.$disconnect();
  });
