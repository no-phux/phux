# Build on phux

One page for a program that drives a phux server, or reports status into a
pane. Each entry links the page that specifies it; any promise here is
quoted from there.

## Entry points

- [Versioned `--json`](./scripting.md#json-output-and-errors). Pass `--json`
  on a command whose `--help` lists it. The generated text is the
  [CLI reference](./reference/cli.md).
- [MCP](./agents.md#mcp). `phux mcp` serves tools over stdio. The
  [parity table](./reference/parity.md) is generated from the tool catalog.
- [`phux watch`](./scripting.md#watching-events). `--json` prints one JSON
  object per line. `--after` resumes from the cursor the previous run
  printed last on stderr.
- [Wire protocol](./protocol.md). Length-prefixed binary frames.
  [Compatibility](./compatibility.md) is the 1.x rule.
  [`wire-schema.json`](./spec/wire-schema.json) is generated from the codec.
<!-- TODO: the page titled "Any program can report status" is not on main.
     The OSC 7501 entry below links program-protocol.md until that page exists. -->
- [OSC 7501](./program-protocol.md). A program writes one escape sequence.
  It needs no phux library and no socket.
- [Plugins](./plugins.md). A directory with a `phux-plugin.toml` manifest.
- [Agent integrations](./agents.md#agent-setup). The table there includes
  Claude Code, pi, Oh My Pi, OpenCode, Grok, and any MCP client.
- [Calling phux from a program](./agent-quickstart.md#call-phux-from-a-program)
  and [embedding](./embedding.md). Read these for what a library would be,
  and for the SDK status below.

```sh
phux watch --json --after "$cursor" @7
phux mcp --schema
```

## Agent state

Agent state is evidence-ranked; screen is the floor.

[Coding agents](./agents.md#agent-states) names the order the server keeps:
what the agent declares through its integration or `phux agent set`, the
events its integration reports, an OSC 7501 report, hooks, the program in
the foreground, and finally the screen matched against built-in rules.
Screen rules ship for Claude Code, Codex, OpenCode, pi, Oh My Pi, Grok,
Amp, Cursor, Gemini CLI, Goose, and Aider. A screen that matches no rule
reads as `idle`, never `blocked`. `phux agent explain` prints the evidence
for one pane.

[Precedence](./program-sequences.md#precedence) writes that order as a
numbered list. [Precedence](./program-sequences.md#precedence) says that
while a pane has an OSC 7501 record and no live session, phux ignores
hooks, the foreground program, and the screen.

It has not been measured on a live Claude Code, Codex, or OpenCode session,
with or without an integration installed.

## Not shipped

There is no public SDK. [Call phux from a program](./agent-quickstart.md#call-phux-from-a-program)
says this repository ships no Python or JavaScript package for driving
phux. `phux-client-core` and `phux-client-runtime` set `publish = false`.
[Embedding](./embedding.md) says none of the embed crates is on crates.io
yet. [Releasing](./releasing.md#npm-and-cratesio) says `phux-protocol` is
the crate published to crates.io, and every other crate is `publish = false`.
The npm packages in that table (`@phux/pi`, `@phux/opencode`, `@phux/omp`)
are agent integrations. `@phux/integration-runtime` is `"private": true`
([`integrations/runtime/package.json`](../integrations/runtime/package.json)).

There is no JSON socket. A client speaks the binary frames in
[Transports](./protocol.md#transports), or it spawns the `phux` process and
reads `--json` on that process's stdout. The [CLI reference](./reference/cli.md)
has no `phux api` command.

There is no example application. [examples/agents](../examples/agents/README.md)
is scripts that spawn the `phux` binary. [Plugin examples](./plugins.md#examples)
are manifests. The embed example in [embedding](./embedding.md) draws a
terminal.

There is no plugin index. `phux plugin install` takes a git URL, a local
directory, or a tarball ([Managing plugins](./plugins.md#managing-plugins)).

## Version rules

The wire protocol and `--json` output each have a written compatibility
rule. The product version follows its own.

[Compatibility](./compatibility.md):

> The wire protocol is `1.0`. Any 1.x client works with any 1.x server, older
> or newer.

[Making a change compatible](./compatibility.md#making-a-change-compatible):

> Do not bump the major for an additive change; a shipped bit is never
> withdrawn or repointed.

[JSON output and errors](./scripting.md#json-output-and-errors):

> `--json` output is versioned. Each document has a `schema_version` that
> changes only when a key is removed, renamed, or retyped. New keys appear
> without a bump, so ignore keys you don't know.

On failure, stdout is empty and stderr is one JSON object. Branch on
`error.code`. `phux --capabilities --json` prints the code list. Exit codes
are the [exit-code reference](./reference/exit-codes.md).

`phux watch --json` is outside that per-document rule. Its generated help
says each line is one JSON object and that there is no per-line
`schema_version` ([`phux watch`](./reference/cli.md#phux-watch)).

[Versioning](./releasing.md#versioning) covers the product version:

> Pre-1.0: `fix:` bumps the patch, `feat:` bumps the minor, and a breaking
> change also bumps the minor.

There is no SDK compatibility promise, because there is no public SDK.

## Limits of the pages above

`phux watch` takes one target. [Watching events](./scripting.md#watching-events)
shows a pane (`@7`). The generated help says omitting the target watches
the most-recently-focused session ([`phux watch`](./reference/cli.md#phux-watch)).
Neither page documents a fleet-wide subscription.

## llms.txt

The builder index checked into this repository is [llms.txt](./llms.txt).
[https://docs.phux.sh/llms.txt](https://docs.phux.sh/llms.txt) is a different
file, kept in `no-phux/website` at `public/llms.txt`. Nothing in this
repository generates that hosted file.
[https://docs.phux.sh/llms-full.txt](https://docs.phux.sh/llms-full.txt) is
every hosted page.
