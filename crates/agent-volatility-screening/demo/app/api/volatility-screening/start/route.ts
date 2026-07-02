import { NextResponse } from "next/server";
import { startVolatilityScreening } from "@/lib/store";
import type { VolatilityScreeningConfig } from "@/lib/volatilityscreening-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

const SECONDS_PER_YEAR = 365 * 24 * 3600; // 31_536_000

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const market = String(body.market ?? "CC-USDC");
    const windowN = num(body.window, "window");
    const pollSecs = num(body.pollSecs, "pollSecs");
    const startingPrice = num(body.startingPrice, "startingPrice");
    const periodsPerYearRaw = body.periodsPerYear === undefined
      ? 0
      : num(body.periodsPerYear, "periodsPerYear");

    if (!Number.isInteger(windowN) || windowN < 2) {
      return NextResponse.json({ error: "window must be an integer >= 2" }, { status: 400 });
    }
    if (pollSecs <= 0) return NextResponse.json({ error: "pollSecs must be > 0" }, { status: 400 });
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });

    const periodsPerYear = periodsPerYearRaw > 0 ? periodsPerYearRaw : SECONDS_PER_YEAR / pollSecs;

    const config: VolatilityScreeningConfig = {
      market,
      window: windowN,
      pollSecs,
      periodsPerYear,
      startingPrice,
    };
    const state = startVolatilityScreening(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
