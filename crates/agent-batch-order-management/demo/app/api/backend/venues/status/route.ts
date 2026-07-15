import { forwardBackend } from "@/lib/proxy-backend";


/** GET → apps/api `/api/venues/status`. */

export async function GET(): Promise<Response> {


  return forwardBackend("/api/venues/status");


}
