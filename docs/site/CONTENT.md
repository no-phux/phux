# phux site content contract

The product site lives on `phux.sh`; documentation lives on `docs.phux.sh`.
They share an identity, not a reading task. Marketing demonstrates why phux
exists. Documentation helps someone choose, use, troubleshoot, or build on it.
Canonical technical facts remain in the repository documentation. The site
publishes those sources rather than maintaining a second technical manual.

## The proposition

phux is a programmable terminal runtime: a background server owns terminals;
clients and automation control them through a public protocol, locally or
across machines. Shared views are one consequence, not the whole proposition.

The display name, category, headline, and description live in `src/lib/site.ts`.
Site chrome, page titles, demos, and the generated social card read that identity.
Changing it does not rename commands, packages, domains, protocol identifiers,
or the canonical documentation. A product rename must migrate those deliberately.

Explain useful behavior before resource kinds and wire layers. Distinguish
emitted agent lifecycle state from terminal-based detection. Client rendering
state is not a second running process. The durable coordinator is planned,
not a shipped reason to install.

## Readers and language

1. A person trying phux needs supported platforms, an install command, a first
   successful session, and clear persistence boundaries.
2. A person running coding agents needs an integration chooser, setup, a way to
   verify the connection, and safe handoff between human and agent.
3. A returning user needs direct navigation, searchable commands, accurate
   errors, and a recovery path.
4. Integrators and contributors need the exact protocol and implementation
   reference, without marketing claims in normative text.

Follow the [editorial rules](../CONVENTIONS.md#writing). Keep the wordmark
lowercase and headings in sentence case. Name the channel and version when
behavior depends on them; “this tree” is not a release identifier.

Short guides link to the full reference. Do not make architecture or historical
decisions a prerequisite for starting a terminal.

## Documentation navigation

`/overview` is the primary documentation home. `/docs` remains the complete
addressable documentation index, not a competing start button. Navigation
metadata is generated in `scripts/sync-docs.ts`. Task folders reference the
existing documents, preserving public URLs and their canonical sources.

- **Start here:** supported installation, first terminal, essential concepts,
  and the translation from tmux.
- **Use phux:** terminal interaction, configuration, desktop/browser interfaces,
  and recording.
- **Run coding agents:** a concrete first-success guide, then host-specific
  integrations and MCP.
- **Connect machines:** SSH setup first, then pairing, routes, and recovery.
- **Performance & comparisons:** measured results and product fit, clearly
  separated from one another.
- **Troubleshoot & maintain:** diagnosis, safe recovery, and operational detail.
- **Reference:** generated command/configuration facts and terminal automation.
- **Protocol:** exact interoperability requirements and a worked tutorial.
- **Build & contribute:** client development, architecture, and design records.

## Page contracts

A task guide answers, in order:

1. What will I accomplish?
2. What platform, version, access, or prior setup do I need?
3. What do I do, and on which machine or terminal?
4. What should I observe when it works?
5. What do I check if it does not?
6. What is the next useful task or deeper reference?

The article header shows one complete useful summary, not a truncated sentence
followed by an expandable duplicate. Source provenance is available without
interrupting the task. Descriptive link labels explain destinations. Keep
existing deep-link headings stable or migrate their callers.

The overview routes readers by task and illustrates the server/client model.
Its title, summary, and task links come from `OVERVIEW` in `src/lib/site.ts`,
shared with the generated search representation.

Screenshots and diagrams explain something the prose alone makes hard to see.
Provide meaningful alternatives; motion is never required to understand a
workflow. Do not present planned or source-build-only interfaces as equivalent
to an available install.

## Performance and comparisons

Honest comparison tables are encouraged. Unsupported scorecards and universal
“fastest” claims are not. Include tmux, Herdr, and cmux where the question is
meaningful, with exact versions and an explicit system boundary.

PTY-byte echo is not keystroke-to-pixel latency. A daemon's RSS is not a whole
GUI application's memory footprint. Local loopback is not a remote-network
measurement. Missing data, unsupported tasks, and failed measurements are
separate states, never zeros. Keep product-fit judgments separate from measured
results. Publish losses as readily as wins.

Every new measured result needs its date, hardware, OS, configuration, sample
count, commands, and retained raw evidence. Explain variability and limitations.
Historical summaries without original samples remain labeled historical;
rerunning a current harness does not recreate missing evidence from an old run.
Charts must have readable units, direct labels, an accessible table, and a
methodology link. Do not hide conditions behind tooltips.

## Product landing and demo

The landing at `src/pages/index.astro` stays minimal: headline, one static
terminal picture, three capabilities, installation, and the iPhone beta and
release-update signups. Add a section only when it replaces one.

`<HeroDemo client:load>` renders the terminal picture and owns the hosted
terminal dialog. Any `[data-demo-launch]` link opens the dialog; without
JavaScript the link falls through to the standalone shell at `/embed`. The
picture is a still, not simulated live output, and the landing downloads no
WASM and allocates no hosted session until the visitor opens the dialog. The
anonymous edge tour is a curated, OS-less shell in a Durable Object; the
optional authenticated Linux tab runs the native container. Label both
runtimes and their different capabilities. The edge tour has no network or
processes; do not imply it is a general shell. The live shell supports
independent split panes with visible controls and keyboard shortcuts; the
browser interaction contract lives in
[the web client guide](../consumers/web.md#in-the-page).

Closing the dialog or switching runtime releases the hosted session, including
an in-flight attach; hosted demo sessions are intentionally disposable.
JavaScript-disabled visitors still see the picture, the standalone-shell link,
and installation. An empty `PUBLIC_PHUX_DEMO_WS` produces an explicit not-configured state; an unreachable backend gets an
actionable error and recovery controls. Neither failure disables the
installation path.

Accessibility, light/dark reading, code copying, responsive layout, and visual
roles are specified in `DESIGN_SYSTEM.md`. Verify them in the built browser
surface, not by assuming the framework provides them.
