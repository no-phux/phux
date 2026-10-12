// Which Cloudflare account these stacks may touch.
//
// Locally the package scripts pass `--profile phux`. Alchemy still prefers
// CLOUDFLARE_API_TOKEN/CLOUDFLARE_ACCOUNT_ID from the environment over any
// profile, so a token exported for another project would silently win. This
// guard runs before anything is planned and fails on every account but phux's.
import * as Cloudflare from "alchemy/Cloudflare";
import * as Effect from "effect/Effect";

export const PHUX_ACCOUNT_ID = "c3f165d818338f3e550df0e69c818e45";

export const requirePhuxAccount = Effect.gen(function* () {
  const { accountId } = yield* yield* Cloudflare.CloudflareEnvironment;
  if (accountId !== PHUX_ACCOUNT_ID) {
    return yield* Effect.die(
      new Error(
        `Refusing to run against Cloudflare account ${accountId}: phux only deploys to ${PHUX_ACCOUNT_ID}. ` +
          "Check `alchemy profile current` and unset any CLOUDFLARE_API_TOKEN/CLOUDFLARE_ACCOUNT_ID from another project.",
      ),
    );
  }
});
