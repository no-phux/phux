# phux site content contract

The product site lives on `phux.sh`; documentation lives on `docs.phux.sh`.
They share an identity, not a reading task. Marketing demonstrates why phux
exists. Documentation helps someone choose, use, troubleshoot, or build on it.
Canonical technical facts remain in the repository documentation. The site
publishes those sources rather than maintaining a second technical manual.

## The proposition

People, applications, scripts, and coding agents can inspect and control the
same running terminals. A background server owns the shells. Terminal and
desktop interfaces are views of those running processes, not separate sessions
that must be synchronized by copying logs.

Explain that useful behavior before introducing resources, wire layers, or
engine implementation. Do not imply clients have no local rendering state, or
promise compatibility with every future terminal protocol. Distinguish emitted
agent lifecycle state from terminal-based detection.

## Readers and language

1. A person trying phux needs supported platforms, an install command, a first
   successful session, and clear persistence boundaries.
2. A person running coding agents needs an integration chooser, setup, a way to
   verify the connection, and safe handoff between human and agent.
3. A returning user needs direct navigation, searchable commands, accurate
   errors, and a recovery path.
4. Integrators and contributors need the exact protocol and implementation
   reference, without marketing claims in normative text.

Use direct, specific prose. Explain unfamiliar terms on first use. Keep the
wordmark lowercase; use normal sentence case for navigation and headings.
Prefer “Open the browser client” to “Choose your glass,” and “Configure phux”
to a visible repository filename. Name the product, channel, and version when
behavior depends on them; “this tree” is not a release identifier.

Do not dilute the technical detail. Put it at the point where it is useful.
Short user guides link to the exact reference; they do not copy its full
schema. Architecture and historical decisions are not onboarding prerequisites.

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

The overview offers a clear first action, explains shared terminals visually
and in text, and routes readers by task. It is not a second feature pitch or
an exhaustive list of protocol layers. Its search representation must name the
same tasks and destinations as the rendered page.

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

The marketing landing at `src/pages/index.astro` uses
`<MultiplexShowcase client:load>` to explain split views, shared clients, and
detach/reattach. The sequence is shared-terminal proposition, interactive
diagram with a launch-gated real terminal, agent-attention demonstration,
capabilities, then installation. Guide links lead into the reader paths above.
Update this contract when changing that sequence.

The opening panel is explicitly a diagram, not simulated live output. It works
without downloading WASM or allocating a hosted session. Opening the terminal
dialog loads the actual phux web client: the anonymous edge tour is a curated,
OS-less shell in a Durable Object; the optional authenticated Linux tab runs
the native container. Label both runtimes and their different capabilities.
The edge tour has no network or processes; do not imply it is a general shell.

Closing the dialog or switching runtime releases the hosted session, including
an in-flight attach. The diagram describes normal phux continuity; hosted demo
sessions are intentionally disposable. JavaScript-disabled visitors still see
the diagram, explanation, and installation links. An empty `PUBLIC_PHUX_DEMO_WS`
produces an explicit not-configured state; an unreachable backend gets an
actionable error and recovery controls. Neither failure disables the diagram
or installation path.

Accessibility, light/dark reading, code copying, responsive layout, and visual
roles are specified in `DESIGN_SYSTEM.md`. Verify them in the built browser
surface, not by assuming the framework provides them.
