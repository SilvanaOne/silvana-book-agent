import { NextResponse } from "next/server";
import { rerun } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { const s = rerun(); return NextResponse.json({ ok: !!s }); }
