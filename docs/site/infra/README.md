# phux-site infrastructure (Alchemy)

`alchemy.run.ts` declares the `phux-site` Worker (phux.sh + docs.phux.sh):
the `../host/index.ts` router, the `../dist` assets, the `TelemetryDO`
Durable Object, and a service binding to the private `phux-ops-analytics`
Worker. `site-deploy.yml` deploys it after every site build. The demo Worker
(`../worker`) is not here yet.

```sh
bun install
bun run plan      # alchemy plan  --stage prod --profile phux
bun run deploy    # alchemy deploy --stage prod --profile phux (CI does this)
```

- **Profile:** local commands use the `phux` [Alchemy profile](https://alchemy.run/environments/profiles)
  (`alchemy profile show --profile phux`). `account.ts` refuses every
  Cloudflare account but phux's, including one picked up from exported
  `CLOUDFLARE_*` variables. CI uses `CLOUDFLARE_API_TOKEN` /
  `CLOUDFLARE_ACCOUNT_ID`.
- **State:** none is shared. Each run starts empty and re-adopts the live
  Worker, which Alchemy recognises by its `alchemy:*` tags. Durable Object
  migrations and custom domains are reconciled against the live Worker, not
  state. The Worker is `retain()`ed, so removing it from code never deletes it.
- **Secrets** (`MEMBER_KEY`, `MEMBER_CLAIM_KEY`, `TELEMETRY_INGEST_KEY`,
  `ASC_*`) are put on the Worker with `bunx wrangler secret put NAME --name
  phux-site` and are kept across deploys (`../DEPLOY.md`).
- Only stage `prod` exists. Never `alchemy destroy`.
