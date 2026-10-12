// Local development for the demo Worker: edge mode only (SessionDO + the
// phux-edge WASM) in local workerd at http://localhost:8787. Native mode needs
// the container image and Workers Containers, so it stays off here.
//
//   bun run dev:worker   (from docs/site)
//
// Secrets come from infra/.env when set, else throwaway dev values.
import * as Alchemy from "alchemy";
import * as Cloudflare from "alchemy/Cloudflare";
import * as State from "alchemy/State";
import * as Config from "effect/Config";
import * as Effect from "effect/Effect";
import * as Redacted from "effect/Redacted";
import { requirePhuxAccount } from "./account.ts";

const secret = (name: string) =>
  Config.Redacted(name).pipe(Config.withDefault(Redacted.make(`${name}-dev`)));

export default Alchemy.Stack(
  "phux-demo-dev",
  { providers: Cloudflare.providers(), state: State.localState() },
  Effect.gen(function* () {
    yield* requirePhuxAccount;
    const demo = yield* Cloudflare.Worker("Demo", {
      main: "../worker/src/index.ts",
      compatibility: { date: "2025-05-01", flags: ["nodejs_compat"] },
      env: {
        SESSION: Cloudflare.DurableObject("SESSION", { className: "SessionDO" }),
        GLOBAL_CAP: Cloudflare.DurableObject("GLOBAL_CAP", {
          className: "GlobalCapDO",
        }),
        RATE_LIMIT: Cloudflare.DurableObject("RATE_LIMIT", {
          className: "RateLimitDO",
        }),
        NATIVE_ENABLED: "false",
        ALLOWED_ORIGINS: process.env.ALLOWED_ORIGINS ?? "http://localhost:4321",
        SESSION_TOKEN_SECRET: secret("SESSION_TOKEN_SECRET"),
        AUTH_COOKIE_SECRET: secret("AUTH_COOKIE_SECRET"),
        SYNTHETIC_TOKEN_SECRET: secret("SYNTHETIC_TOKEN_SECRET"),
      },
      dev: { port: 8787 },
    });
    return { url: demo.url };
  }),
);
