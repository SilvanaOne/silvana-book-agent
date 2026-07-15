import { NextResponse } from "next/server";
import { store } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET() {
  const { keyId, pem } = store().key;
  return NextResponse.json({ keyId, pem });
}
