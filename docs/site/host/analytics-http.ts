/** Effect HttpRouter transport for the public member analytics endpoints. */
import { Context, Effect } from "effect";
import {
  HttpRouter,
  HttpServerRequest,
  HttpServerResponse,
} from "effect/unstable/http";
import {
  handleClaim,
  handleJoin,
  type AnalyticsEnv,
} from "./analytics";
import {
  MemberCrypto,
  runAnalyticsBackground,
  type WaitUntil,
} from "./analytics-runtime";

interface AnalyticsRequestContext {
  readonly env: AnalyticsEnv;
  readonly ctx?: WaitUntil;
}

class AnalyticsRequest extends Context.Service<
  AnalyticsRequest,
  AnalyticsRequestContext
>()("phux.site/analytics/AnalyticsRequest") {}

const joinRoute = Effect.fn("analytics.joinRoute")(function*(
  request: HttpServerRequest.HttpServerRequest,
) {
  if (request.method !== "POST") {
    return HttpServerResponse.text("post only", { status: 405 });
  }
  const context = yield* AnalyticsRequest;
  const result = yield* handleJoin(request.source as Request, context.env);
  if (result.background) {
    yield* Effect.sync(() =>
      runAnalyticsBackground(context.ctx, result.background!),
    );
  }
  return HttpServerResponse.fromWeb(result.response);
});

const claimRoute = Effect.fn("analytics.claimRoute")(function*(
  request: HttpServerRequest.HttpServerRequest,
) {
  if (request.method !== "GET") {
    return HttpServerResponse.text("get only", { status: 405 });
  }
  const context = yield* AnalyticsRequest;
  return HttpServerResponse.fromWeb(
    yield* handleClaim(request.source as Request, context.env),
  );
});

const Routes = HttpRouter.addAll([
  HttpRouter.route("*", "/api/join", joinRoute),
  HttpRouter.route("*", "/api/claim", claimRoute),
]);

const webApp = HttpRouter.toWebHandler(Routes, { disableLogger: true });

/** The sole Promise adapter for the analytics HTTP sub-application. */
export function handleAnalyticsHttp(
  request: Request,
  env: AnalyticsEnv,
  ctx?: WaitUntil,
): Promise<Response> {
  const context = Context.make(AnalyticsRequest, { env, ctx }).pipe(
    Context.add(MemberCrypto, MemberCrypto.service),
  );
  return webApp.handler(request, context);
}
