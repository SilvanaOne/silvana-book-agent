import { forwardBackend } from "@/lib/proxy-backend";

/** GET → apps/api `/api/audit/entries` */
export async function GET(req: Request): Promise<Response> {
  const u = new URL(req.url);


  const limit = u.searchParams.get("limit") ?? "";

  const q = limit ? `?limit=${encodeURIComponent(limit)}` : "";

  return forwardBackend(`/api/audit/entries${q}`);

}
