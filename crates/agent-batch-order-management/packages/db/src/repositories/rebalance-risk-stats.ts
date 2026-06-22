import { Decimal } from "@prisma/client/runtime/library";
import { prisma } from "../client.js";

function startOfUtcDay(d: Date): Date {
  return new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate(), 0, 0, 0, 0));
}

/** Достаёт `output.portfolioEngine.estimatedNotional` из JSON job. */
function estimatedNotionalFromOutput(output: unknown): Decimal | null {
  if (!output || typeof output !== "object" || Array.isArray(output)) return null;
  const root = output as Record<string, unknown>;
  const pe = root.portfolioEngine ?? root.portfolio_engine;
  if (!pe || typeof pe !== "object" || Array.isArray(pe)) return null;
  const est = (pe as Record<string, unknown>).estimatedNotional ?? (pe as Record<string, unknown>).estimated_notional;
  if (typeof est !== "string") return null;
  try {
    return new Decimal(est);
  } catch {
    return null;
  }
}

/**
 * Сумма `estimatedNotional` успешных `live_execute` за UTC-день (по факту сохранённого `output`).
 * Учитываются только строки со статусом `completed`.
 */
export async function sumCompletedLiveEstimatedNotionalUtcDay(
  portfolioId: string,
  options?: Readonly<{ excludeJobId?: string; at?: Date }>,
): Promise<Decimal> {
  const ref = options?.at ?? new Date();
  const dayStart = startOfUtcDay(ref);

  const rows = await prisma.rebalanceJob.findMany({
    where: {
      portfolioId,
      dryRun: false,
      mode: "live_execute",
      status: "completed",
      createdAt: { gte: dayStart },
      ...(options?.excludeJobId ? { id: { not: options.excludeJobId } } : {}),
    },

    select: { output: true },
  });

  let sum = new Decimal(0);
  for (const r of rows) {
    const n = estimatedNotionalFromOutput(r.output);
    if (n) sum = sum.plus(n);
  }


  return sum;
}
