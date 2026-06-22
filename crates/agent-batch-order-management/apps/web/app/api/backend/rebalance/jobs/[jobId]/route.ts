import { forwardBackend } from "@/lib/proxy-backend";

type RouteCtx = { params: Promise<{ jobId: string }> };

/** GET → apps/api `/api/rebalance/jobs/:id?detail=` */
export async function GET(req: Request, ctx: RouteCtx): Promise<Response> {
  const { jobId } = await ctx.params;

  const u = new URL(req.url);

  const detail = u.searchParams.get("detail") ?? "";

  const q = detail ? `?detail=${encodeURIComponent(detail)}` : "";

  return forwardBackend(`/api/rebalance/jobs/${encodeURIComponent(jobId)}${q}`);
}
