import { NextResponse } from "next/server";
import { resetStore } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { resetStore(); return NextResponse.json({ ok: true }); }
