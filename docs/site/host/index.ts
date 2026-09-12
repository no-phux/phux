/**
 * phux-site host router.
 *
 * Static assets stay in dist/; this worker only decides which host a path
 * belongs on. See host/routes.ts for the split.
 */
import { routeRequest } from "./routes";

export interface Env {
  ASSETS: { fetch(input: Request): Promise<Response> };
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const routed = routeRequest(url.hostname, url);
    if (routed.kind === "redirect") {
      return Response.redirect(routed.location, routed.status);
    }
    return env.ASSETS.fetch(request);
  },
};
