import { forwardBackend } from "@/lib/proxy-backend";

type RouteCtx = {
  params: Promise<{ id: string }>;

};

/** GET → apps/api `/api/portfolio/:id` */
export async function GET(_req: Request, ctx: RouteCtx): Promise<Response> {

  const { id } = await ctx.params;

  return forwardBackend(`/api/portfolio/${encodeURIComponent(id)}`);
}
