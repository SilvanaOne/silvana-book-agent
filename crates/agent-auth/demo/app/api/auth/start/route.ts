import { NextResponse } from "next/server";
import { startAuth } from "@/lib/store";
import type { AuthConfig } from "@/lib/auth-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function bool(v: unknown, name: string): boolean {
  if (typeof v === "boolean") return v;
  if (typeof v === "string") return v === "true" || v === "1";
  throw new Error(`${name} must be boolean`);
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const config: AuthConfig = {
      role: String(body.role ?? "trader"),
      ttlSecs: num(body.ttlSecs, "ttlSecs"),
      autoRefreshEnabled: bool(body.autoRefreshEnabled ?? true, "autoRefreshEnabled"),
      autoRefreshIntervalSecs: num(body.autoRefreshIntervalSecs, "autoRefreshIntervalSecs"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.role.trim().length === 0)
      return NextResponse.json({ error: "role must be non-empty" }, { status: 400 });
    if (config.ttlSecs <= 0)
      return NextResponse.json({ error: "ttlSecs must be > 0" }, { status: 400 });
    if (config.autoRefreshIntervalSecs <= 0)
      return NextResponse.json({ error: "autoRefreshIntervalSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0)
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startAuth(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
