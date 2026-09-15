/** Effect services and the Cloudflare runtime adapter for public analytics. */
import { Context, Effect, Layer, ManagedRuntime, Schema } from "effect";

export interface WaitUntil {
  waitUntil(promise: Promise<unknown>): void;
}

export interface AnalyticsBinding {
  fetch(input: Request): Promise<Response>;
}

export class CryptoFailure extends Schema.TaggedError<CryptoFailure>()(
  "CryptoFailure",
  { cause: Schema.Defect() },
) {}

export class HttpFailure extends Schema.TaggedError<HttpFailure>()(
  "HttpFailure",
  { cause: Schema.Defect(), url: Schema.String },
) {}

export class BodyReadFailure extends Schema.TaggedError<BodyReadFailure>()(
  "BodyReadFailure",
  { reason: Schema.Literals(["malformed", "too-large"]) },
) {}

export class MemberCrypto extends Context.Service<
  MemberCrypto,
  {
    readonly hmacHex: (
      key: string,
      message: string,
    ) => Effect.Effect<string, CryptoFailure>;
    readonly timingSafeEqual: (
      left: string,
      right: string,
    ) => Effect.Effect<boolean, CryptoFailure>;
  }
>()("phux.site/analytics/MemberCrypto") {
  static readonly service: MemberCrypto["Service"] = {
    hmacHex: Effect.fn("MemberCrypto.hmacHex")(function*(key, message) {
      const signature = yield* Effect.tryPromise({
        try: async () => {
          const cryptoKey = await crypto.subtle.importKey(
            "raw",
            new TextEncoder().encode(key),
            { name: "HMAC", hash: "SHA-256" },
            false,
            ["sign"],
          );
          return crypto.subtle.sign(
            "HMAC",
            cryptoKey,
            new TextEncoder().encode(message),
          );
        },
        catch: (cause) => new CryptoFailure({ cause }),
      });
      return bytesToHex(new Uint8Array(signature));
    }),
    timingSafeEqual: Effect.fn("MemberCrypto.timingSafeEqual")(function*(
      left,
      right,
    ) {
      return yield* Effect.tryPromise({
        try: async () => {
          const encoder = new TextEncoder();
          const [leftHash, rightHash] = await Promise.all([
            crypto.subtle.digest("SHA-256", encoder.encode(left)),
            crypto.subtle.digest("SHA-256", encoder.encode(right)),
          ]);
          const subtle = crypto.subtle as SubtleCrypto & {
            timingSafeEqual?: (a: ArrayBuffer, b: ArrayBuffer) => boolean;
          };
          if (subtle.timingSafeEqual) {
            return subtle.timingSafeEqual(leftHash, rightHash);
          }
          // Bun lacks Workers' timingSafeEqual. Hashing fixes the length; this
          // full-buffer fallback keeps local tests behaviorally equivalent.
          const leftBytes = new Uint8Array(leftHash);
          const rightBytes = new Uint8Array(rightHash);
          let difference = 0;
          for (let index = 0; index < leftBytes.length; index += 1) {
            difference |= leftBytes[index] ^ rightBytes[index];
          }
          return difference === 0;
        },
        catch: (cause) => new CryptoFailure({ cause }),
      });
    }),
  };

  static readonly layer = Layer.succeed(MemberCrypto, MemberCrypto.service);
}

export class AnalyticsHttp extends Context.Service<
  AnalyticsHttp,
  {
    readonly execute: (
      request: Request,
      binding?: AnalyticsBinding,
    ) => Effect.Effect<Response, HttpFailure>;
  }
>()("phux.site/analytics/AnalyticsHttp") {
  static readonly layer = Layer.succeed(AnalyticsHttp, {
    execute: Effect.fn("AnalyticsHttp.execute")(function*(
      request: Request,
      binding?: AnalyticsBinding,
    ): Effect.fn.Return<Response, HttpFailure> {
      return yield* Effect.tryPromise({
        try: (signal) =>
          binding ? binding.fetch(request) : fetch(request, { signal }),
        catch: (cause) => new HttpFailure({ cause, url: request.url }),
      });
    }),
  });
}

export const AnalyticsRuntimeLayer = Layer.mergeAll(
  MemberCrypto.layer,
  AnalyticsHttp.layer,
);

// This runtime owns only isolate-safe, stateless services. Request bindings
// enter separately through HttpRouter context and are never retained globally.
const runtime = ManagedRuntime.make(AnalyticsRuntimeLayer);

export function runAnalytics<A, E>(
  program: Effect.Effect<A, E, MemberCrypto | AnalyticsHttp>,
): Promise<A> {
  return runtime.runPromise(program);
}

export function runAnalyticsBackground(
  ctx: WaitUntil | undefined,
  program: Effect.Effect<unknown, unknown, MemberCrypto | AnalyticsHttp>,
): void {
  const promise = runtime.runPromise(program.pipe(Effect.ignoreCause));
  if (ctx) {
    ctx.waitUntil(promise);
  } else {
    void promise;
  }
}

export const readLimitedBody = Effect.fnUntraced(function*(
  request: Request,
  limit: number,
): Effect.fn.Return<Uint8Array, BodyReadFailure> {
  const declared = Number(request.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > limit) {
    return yield* new BodyReadFailure({ reason: "too-large" });
  }
  return yield* Effect.tryPromise({
    try: async () => {
      if (!request.body) return new Uint8Array();
      const reader = request.body.getReader();
      const chunks: Uint8Array[] = [];
      let length = 0;
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        length += value.byteLength;
        if (length > limit) {
          await reader.cancel();
          throw new BodyReadFailure({ reason: "too-large" });
        }
        chunks.push(value);
      }
      const body = new Uint8Array(length);
      let offset = 0;
      for (const chunk of chunks) {
        body.set(chunk, offset);
        offset += chunk.byteLength;
      }
      return body;
    },
    catch: (cause) =>
      cause instanceof BodyReadFailure
        ? cause
        : new BodyReadFailure({ reason: "malformed" }),
  });
});

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
