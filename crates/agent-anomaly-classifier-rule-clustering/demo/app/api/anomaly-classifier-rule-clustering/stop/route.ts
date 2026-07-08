import { NextResponse } from "next/server";
import { stopAgent } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { stopAgent(); return NextResponse.json({ ok: true }); }
