import { forwardBackend } from "@/lib/proxy-backend";

type RouteCtx = {
  params: Promise<{ id: string }>;
};

/** POST → apps/api `/api/portfolio/:id/rebalance-now` */
export async function POST(req: Request, ctx: RouteCtx): Promise<Response> {
  const { id } = await ctx.params;
  const body = await req.text();

  return forwardBackend(`/api/portfolio/${encodeURIComponent(id)}/rebalance-now`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body,
  });
}
