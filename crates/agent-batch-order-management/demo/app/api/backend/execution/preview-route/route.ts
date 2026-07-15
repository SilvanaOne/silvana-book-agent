

import { forwardBackend } from "@/lib/proxy-backend";


/** POST → apps/api `/api/execution/preview-route`. */

export async function POST(req: Request): Promise<Response> {


  const bodyText = await req.text();


  return forwardBackend("/api/execution/preview-route", {


    method: "POST",

    headers: { "Content-Type": "application/json" },



    body: bodyText,

  });


}
