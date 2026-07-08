import { NextResponse } from "next/server";
import { reloadBlocklist } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function normalizeBlocklist(input: unknown): string[] {
  if (Array.isArray(input)) return input.map((x) => String(x));
  if (typeof input === "string") return input.split(/\r?\n/);
  return [];
}

export async function POST(req: Request): Promise<Response> {
  try {
    const body = (await req.json()) as { blocklist?: unknown };
    const cleaned = normalizeBlocklist(body.blocklist)
      .map((s) => s.trim())
      .filter((s) => s.length > 0 && !s.startsWith("#"));
    if (cleaned.length === 0) return NextResponse.json({ error: "blocklist must contain at least one entry" }, { status: 400 });
    const state = reloadBlocklist(cleaned);
    if (!state) return NextResponse.json({ error: "blocked-party is not running" }, { status: 400 });
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
