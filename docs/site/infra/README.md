# phux site + demo infrastructure (Alchemy)

Two [Alchemy](https://alchemy.run) stacks, both stage `prod` on the phux
Cloudflare account:

| Stack        | File             | Worker                                                     | Deployed by |
| ------------ | ---------------- | ---------------------------------------------------------- | ----------- |
| `phux-site`  | `alchemy.run.ts` | `phux-site`: `../host` router + `../dist` assets + `TelemetryDO`, phux.sh + docs.phux.sh | `site-deploy.yml` |
| `phux-demo`  | `demo.run.ts`    | `phux-demo`: `../worker` (SessionDO/GlobalCapDO/RateLimitDO + edge WASM) + the native `PhuxSessionContainer` application, shell.phux.sh + `phux.sh/auth/*` | `site-deploy-worker.yml`, `site-native-control.yml` |

Both Workers have a service binding to the private `phux-ops-analytics`
Worker, whose stack lives in `no-phux/ops`.

```sh
bun install
bun run plan          # site:  alchemy plan  --stage prod --profile phux
bun run deploy        # site:  alchemy deploy --stage prod --profile phux
bun run plan:demo     # demo (needs Containers read on the token)
bun run deploy:demo   # demo (needs Docker for the native image)
bun run dev:demo      # demo-dev.ts: the demo Worker locally, edge only, :8787
```

- **Profile:** local commands use the `phux` [Alchemy profile](https://alchemy.run/environments/profiles)
  (`alchemy profile show --profile phux`). `account.ts` refuses every
  Cloudflare account but phux's, including one picked up from exported
  `CLOUDFLARE_*` variables. CI uses `CLOUDFLARE_API_TOKEN` /
  `CLOUDFLARE_ACCOUNT_ID`.
- **State:** none is shared. Each run starts empty and re-adopts what is live:
  the Workers by their `alchemy:*` tags, the container application by name
  (`phux-demo-phuxsessioncontainer`). Durable Object migrations, custom domains
  and routes are reconciled against the live Worker, not state. Both Workers
  are `retain()`ed, so removing them from code never deletes them.
- **Durable Objects:** migrations are derived from the `env` binding names.
  Renaming a DO binding or class without Alchemy's `transferredFrom`/rename
  semantics deletes that class's data.
- **Native kill switch:** `NATIVE_ENABLED` comes from `PHUX_NATIVE_ENABLED`
  (default `true`); `site-native-control.yml` redeploys with `false`.
- **Secrets** are put on the Workers with `bunx wrangler secret put NAME
  --name phux-site` / `--name phux-demo` and are kept across deploys
  (`../DEPLOY.md`).
- Only stage `prod` exists. Never `alchemy destroy`.
