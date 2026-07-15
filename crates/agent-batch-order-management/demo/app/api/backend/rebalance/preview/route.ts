import { forwardBackend } from "@/lib/proxy-backend";

/** POST → apps/api `/api/rebalance/preview` */
export async function POST(req: Request): Promise<Response> {
  const body = await req.text();

  return forwardBackend(`/api/rebalance/preview`, {
    method: "POST",

    headers: {

      "content-type": "application/json",

    },

    body,
  });

}
