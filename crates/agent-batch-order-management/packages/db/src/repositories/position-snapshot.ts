import { prisma } from "../client.js";

export async function listLatestSnapshotsForPortfolio(portfolioId: string) {
  const latest = await prisma.positionSnapshot.findFirst({
    where: { portfolioId },
    orderBy: { asOf: "desc" },
    select: { asOf: true },
  });
  if (!latest) return [];
  return prisma.positionSnapshot.findMany({
    where: { portfolioId, asOf: latest.asOf },
    orderBy: { assetSymbol: "asc" },
  });
}
