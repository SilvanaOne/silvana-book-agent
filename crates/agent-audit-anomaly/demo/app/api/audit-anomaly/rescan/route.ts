import { NextResponse } from "next/server";
import { reScan } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { const s = reScan(); return NextResponse.json({ ok: !!s }); }
