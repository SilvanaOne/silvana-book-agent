import { NextResponse } from "next/server";
import { approveOrder } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json().catch(() => ({}))) as Record<string, unknown>;
  const id = typeof body.id === "string" ? body.id : "";
  if (id.length === 0) return NextResponse.json({ error: "id is required" }, { status: 400 });
  const by = typeof body.by === "string" ? body.by : undefined;
  const reason = typeof body.reason === "string" ? body.reason : undefined;
  const res = approveOrder(id, by, reason);
  if (!res.ok) return NextResponse.json({ error: res.error }, { status: 400 });
  return NextResponse.json({ ok: true });
}
