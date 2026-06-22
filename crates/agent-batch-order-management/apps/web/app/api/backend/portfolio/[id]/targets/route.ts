import { forwardBackend } from "@/lib/proxy-backend";

type RouteCtx = {
  params: Promise<{ id: string }>;
};

/** PUT → apps/api `/api/portfolio/:id/targets` */
export async function PUT(req: Request, ctx: RouteCtx): Promise<Response> {
  const { id } = await ctx.params;
  const body = await req.text();

  return forwardBackend(`/api/portfolio/${encodeURIComponent(id)}/targets`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body,
  });
}
