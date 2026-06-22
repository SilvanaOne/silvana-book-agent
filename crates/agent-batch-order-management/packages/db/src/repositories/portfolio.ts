import { Decimal } from "@prisma/client/runtime/library";
import { prisma } from "../client.js";

export async function findPortfolioById(portfolioId: string) {
  return prisma.portfolio.findUnique({
    where: { id: portfolioId },
    include: {
      targets: {
        orderBy: { assetSymbol: "asc" },
      },
    },
  });
}

export async function listPortfolioSummaries() {
  return prisma.portfolio.findMany({
    select: { id: true, name: true },
    orderBy: { createdAt: "asc" },
  });
}

export type ReplacePortfolioTargetInput = {
  assetSymbol: string;
  targetWeight: string;
  minWeight?: string | null;
  maxWeight?: string | null;
  enabled: boolean;
};

export async function replacePortfolioTargets(
  portfolioId: string,
  targets: ReadonlyArray<ReplacePortfolioTargetInput>,
) {
  return prisma.$transaction(async (tx) => {
    await tx.portfolioTarget.deleteMany({ where: { portfolioId } });
    if (targets.length > 0) {
      await tx.portfolioTarget.createMany({
        data: targets.map((t) => ({
          portfolioId,
          assetSymbol: t.assetSymbol,
          targetWeight: new Decimal(t.targetWeight),
          minWeight: t.minWeight != null ? new Decimal(t.minWeight) : null,
          maxWeight: t.maxWeight != null ? new Decimal(t.maxWeight) : null,
          enabled: t.enabled,
        })),
      });
    }
    return tx.portfolioTarget.findMany({
      where: { portfolioId },
      orderBy: { assetSymbol: "asc" },
    });
  });
}

export async function replaceLatestPositionSnapshots(
  portfolioId: string,
  positions: ReadonlyArray<{ assetSymbol: string; qty: string; marketValue: string; price: string }>,
) {
  const asOf = new Date();
  return prisma.$transaction(async (tx) => {
    if (positions.length > 0) {
      await tx.positionSnapshot.createMany({
        data: positions.map((p) => ({
          portfolioId,
          assetSymbol: p.assetSymbol,
          qty: new Decimal(p.qty),
          marketValue: new Decimal(p.marketValue),
          price: new Decimal(p.price),
          asOf,
        })),
      });
    }
    return tx.positionSnapshot.findMany({
      where: { portfolioId, asOf },
      orderBy: { assetSymbol: "asc" },
    });
  });
}
