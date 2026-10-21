# @phux/chief

The phux chief of staff: one durable agent per mesh, built on
[Pi Durable](https://github.com/earendil-works/pi/tree/main/packages/durable),
that works through phux panes and shells and reports back to your phone.
The decision record and the client contract are in
[docs/chief.md](../../docs/chief.md).

| file | role |
| --- | --- |
| `src/main.ts` | `run`, `supervise`, `ensure`, `config` |
| `src/run.ts` | open the Harness, bind the pane, replay history, read input |
| `src/chief.ts` | commands, Harness events → phux stream, held messages, status |
| `src/partial.ts` | fold streamed changes into the in-flight reply |
| `src/envelope.ts` | phone envelopes and pane slash commands |
| `src/line-input.ts` | raw-mode line assembly (no MAX_CANON cap), bracketed paste |
| `src/phux-stream.ts` | the pane's `chief` declaration, tag and AgentSession records |
| `src/tools.ts`, `src/web.ts` | phux tools from `@phux/integration-runtime`, `web_search`, `web_fetch` |
| `src/auth.ts` | provider login driven from any client |
| `src/diagnostics.ts` | error intake for self-correction |
| `src/ensure.ts` | supervision |
| `assets/` | persona and skills |

```sh
npm --prefix ../runtime ci && npm --prefix ../runtime run build
npm ci && npm run gates
```
