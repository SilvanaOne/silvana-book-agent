import { NextResponse } from "next/server";
import { reRotate } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { const s = reRotate(); return NextResponse.json({ ok: !!s }); }
