import { NextResponse } from "next/server";
import { verifyChain } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(): Promise<Response> { const s = verifyChain(); return NextResponse.json({ ok: !!s?.lastVerify?.ok }); }
