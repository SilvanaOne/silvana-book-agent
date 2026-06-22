import { forwardBackend } from "@/lib/proxy-backend";

type RouteCtx = {
  params: Promise<{ id: string }>;
};

/** POST → apps/api `/api/portfolio/:id/imitate-drift` (gated by DEMO_TOOLS=1 on the API side) */
export async function POST(req: Request, ctx: RouteCtx): Promise<Response> {
  const { id } = await ctx.params;
  const body = await req.text();

  return forwardBackend(`/api/portfolio/${encodeURIComponent(id)}/imitate-drift`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body,
  });
}
