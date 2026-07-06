import { NextResponse } from "next/server";
import { reFilter } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { const state = reFilter(); return NextResponse.json({ ok: !!state }); }
