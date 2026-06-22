import { PrismaClient } from "@prisma/client";
import { Decimal } from "@prisma/client/runtime/library";

const prisma = new PrismaClient();

/** Устойчивый UUID для локального порта в dev-сидере (повторный seed делает upsert/update). */
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

  await prisma.portfolioTarget.createMany({
    data: [
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "CC",
        targetWeight: new Decimal("0.4"),
        minWeight: null,
        maxWeight: null,
        enabled: true,
      },
      {
        portfolioId: DEV_PORTFOLIO_ID,
        assetSymbol: "USDC",
        targetWeight: new Decimal("0.6"),
        minWeight: new Decimal("0.1"),
        maxWeight: new Decimal("0.9"),
        enabled: true,
      },
    ],
  });

  await prisma.positionSnapshot.deleteMany({ where: { portfolioId: DEV_PORTFOLIO_ID } });

  const asOf = new Date();
  await prisma.positionSnapshot.createMany({
    data: [
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
