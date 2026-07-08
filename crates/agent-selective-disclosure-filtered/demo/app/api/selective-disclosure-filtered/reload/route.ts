import { NextResponse } from "next/server";
import { reloadHistory } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { const state = reloadHistory(); return NextResponse.json({ ok: !!state }); }
