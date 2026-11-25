# Arrow-free pane-stack study — throwaway

Question: how can Rune make real split-pane membership obvious without disclosure
arrows, redundant single-pane rows or a permanent empty gutter?

From the repository root:

```sh
python3 -m http.server 7147 --bind 127.0.0.1 --directory clients/rune/src/ui/sidebar/prototype-stacks
```

Open `http://127.0.0.1:7147/?variant=A` (the default and selected direction).
`B` and `C` preserve the earlier alternatives for comparison.
The bottom bar switches variants, sidebar width and light/dark appearance.
In A, click the member names to change in-memory focus. B/C retain their
thumbnail and exposed-surface alternatives.

- **A / Inline — selected:** smaller, almost-flat rows. A split row is 38px
  rather than 55px. A 28 × 23px identity stack embeds the group's lead tool mark
  directly in its front surface, with faint layered edges and no separate badge,
  shadow, tilt or exposed deck. Claude, Codex and OpenCode are recognizable in
  one restrained monochrome treatment; ordinary terminals use a neutral prompt.
  Single panes get the flat mark, never a false stack. Member names remain
  directly selectable rather than tiny split-thumbnail targets. Keep
  readable labels, visible attention and plain singles instead of shrinking
  every element. This captures the user's request for depth discovered only
  on close inspection, not a loud 3D treatment.
- **B / Exposed:** selected work becomes a labelled dimensional deck; other
  groups stay single lines.
- **C / Spatial:** pane labels and focus live inside a shallow stacked thumbnail
  of their actual split.

All use the same nine illustrative groups and fourteen resources. The latest
fixtures include a browser-associated surface, a Markdown document and an
unsupported future type alongside terminals. Grouping represents actual fixture
membership, not agent-process parentage or shared tool branding. Geometry is
illustrative, fixed fixture data; it is not a server layout implementation.

This is a browser composition study beside the actual native sidebar module.
It is not imported by Rune, does not connect to a server, run commands, persist
state, or replace the production UI. Native token mappings, arbitrary layouts,
session actions, drag ownership and accessibility require proper implementation
after a direction is selected. CSS depth offsets are proposals, not new
production tokens. Keep this study on its throwaway branch, outside `main`.

## Identity marks

Tool identities are explicit, illustrative per-pane fixture data. The stack
shows the group's lead tool; choosing a member changes in-memory pane focus,
not the group's identity or the split arrangement. Production must use actual
producer metadata and a neutral fallback, never infer a tool from a title or
hard-code routing around these fixture names.

The Claude, Codex and OpenCode SVG paths come from
[Lobe Icons](https://github.com/lobehub/lobe-icons/tree/master/packages/static-svg/icons)
(`claude.svg`, `codex.svg`, `opencode.svg`), under the included
[MIT license](marks/LICENSE). The terminal prompt is a local neutral mark.
All marks are local files; the running prototype makes no third-party requests.

## Resource-first information architecture

One compact row consumes identity, name, status, useful secondary detail and
explicit members. Resource kind supplies a neutral default; known application
identity refines the mark. It does not decide grouping, capabilities or input.
Adding an identity is a registry entry, not a provider-specific row renderer.
Search includes the identity, kind and secondary detail as well as visible names.

Try `?variant=A&resource=browser`, `&resource=docs` or `&resource=future`.
These are honest illustrative placeholders, not browser support or real content:
the generic preview does not execute a terminal, expose input or claim a
renderer for the unknown type. Unsupported resources remain visible and neutral,
never silently rendered as a terminal or marked healthy.

For native implementation, reuse `ResourceRecord`, the kind catalog,
`Desk::addressable()` and sidebar `Item` grouping rather than defining a second
resource system. The current addressable projection only admits supported
terminal/document kinds and their available mirrors; unknown kinds are not
currently surfaced there. Extending presentation must retain that distinction
and preserve capability gating, trust, remembered focus and the server's real
arrangement. Provider identity must come from existing agent metadata, not names.
Browser mirrors already exist as accessibility-tree and pixel-surface resources,
not a separate Browser wire kind. Their producer parent does not establish a
browser tab group. Live URLs, favicons and page-pair identity are not currently
structured inventory fields and must not be fabricated from titles or parentage.
