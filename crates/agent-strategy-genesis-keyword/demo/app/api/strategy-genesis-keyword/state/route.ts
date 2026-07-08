import { NextResponse } from "next/server";
import { ensureLoaded, snapshot } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function GET(): Promise<Response> { ensureLoaded(); return NextResponse.json(snapshot()); }
