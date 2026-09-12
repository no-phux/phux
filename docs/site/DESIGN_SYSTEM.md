# phux site design system

The site has one active visual mode, selected by `SITE.designMode` in
`src/lib/site.ts` and exposed as `data-design` on both HTML shells. Change that
single value to switch modes; mode-specific values belong in the token block in
`src/styles/global.css`, not in components.

The active mode is `terminal`. The marketing page is a product site that
contains a terminal; it is not itself a terminal costume. Structure still
comes from phux's renderer: exact rules, content-owned interiors, explicit
overflow, and semantic state. The live island is the product. Chrome around
it stays quiet.

### Type roles

- IBM Plex Sans for marketing, docs chrome, and long-form reading.
- IBM Plex Mono for commands, selectors, protocol symbols, and the
  live island.
- Nav is sentence case. The wordmark stays lowercase `phux`.
- Hierarchy comes from size, weight, rule, and position.

### Color roles

- `--color-bg`: page canvas.
- `--color-surface` / `--color-surface-raised`: grouped and interactive surfaces.
- `--color-fg`: primary text.
- `--color-muted`: supporting text.
- `--color-rule`: quiet structural separation.
- `--color-accent` (`#7aa2f7`): identity, selection, and current position.
- `--color-signal` (`#9ece6a`): live, actionable, and successful state.
- `--color-attention` (`#ff9e64`): blocked work or required human attention.
- `--color-warn` (`#e0af68`): caution and section markers.
- `--color-error` (`#f7768e`): persistent or fatal failure.
- `--color-cyan` (`#7dcfff`): completed work and wire activity.
- State must remain distinguishable by glyph, weight, or position without color.

### Shape and spacing

- All radii are zero. Surfaces meet on exact rules; nothing floats in a lozenge.
- Shadows and ornamental gradients are prohibited.
- Light rules divide peers. A heavy or accent rule marks focus and ownership.
- `--page-gutter`: fluid page edge spacing. The canvas is flat; the live
  terminal is the texture, not a page grid.
- `--content-wide`: landing and terminal maximum width.
- `--reading-width`: long-form prose measure.

### Interaction rules

- Navigation and route rows use explicit markers and rule changes, not cards.
- Prose links use accent color and are underlined by default; hover/focus
  strengthens the underline.
- Focus always has a visible two-pixel square outline.
- Motion is limited to state changes. No entrance motion or decorative drift.

### Technical text

- Inline code names a command, symbol, path, value, or short literal inside a
  sentence. It is monospace but does not get a boxed background.
- Fenced code is executable, copyable, multiline, or byte-structural content.
  It progressively enhances to `@pierre/diffs` File with Pierre Dark syntax
  highlighting, line numbers, and contained scrolling; the static block is the
  no-JavaScript and failure fallback.
- Tables remain semantic tables and sit in keyboard-focusable scroll regions.
- Status, warning, security, and implementation notes use bordered callouts;
  ordinary emphasis does not.

## Content architecture

The site is organized by reader intent rather than repository layout:

1. **Start**: decide, install, and complete one successful session.
2. **Guides**: accomplish a human, agent, integration, or remote-access task.
3. **Reference**: exact generated commands, defaults, schemas, paths, and codes.
4. **Protocol**: normative interoperability requirements and wire encoding.
5. **Architecture**: explanatory implementation structure and operations.
6. **Decisions**: historical rationale, searchable and linkable but collapsed
   to one index in default navigation.

Progressive disclosure is mandatory:

- Section indexes answer where to go next; they do not restate child pages.
- Page descriptions are short scan-oriented metadata. Full upstream summaries
  remain available through disclosure when longer.
- Task pages lead with the working path, then constraints and exact reference.
- Monolithic upstream contracts may be published as exact heading slices. The
  source file remains authoritative and every slice links to its immutable SHA.
- A normal guide should target 500-1,500 words. Pages over 3,000 words need a
  task split or a clear reason to remain exhaustive reference.
- Generated and normative text is reorganized or transformed for presentation,
  never paraphrased into a competing source of truth.

### Responsive rules

- Mobile starts at the page gutter and uses one-column surfaces.
- The docs desktop sidebar is suppressed from 768-1023px so tablet readers get
  the full width and the mobile navigation trigger.
- Long code and tables scroll inside their own surface; the page never scrolls
  horizontally.
