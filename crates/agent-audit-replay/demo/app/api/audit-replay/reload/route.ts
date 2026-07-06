import { NextResponse } from "next/server";
import { reload } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { const s = reload(); return NextResponse.json({ ok: !!s }); }
