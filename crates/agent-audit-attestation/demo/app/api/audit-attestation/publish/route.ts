import { NextResponse } from "next/server";
import { manualPublish } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { const state = manualPublish(); return NextResponse.json({ ok: !!state }); }
