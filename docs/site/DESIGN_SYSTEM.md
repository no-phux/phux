---
audience: contributors
stability: stable
last-reviewed: 2026-10-14
---
# phux site design system

**TL;DR.** The landing pairs a quiet product tour with an on-demand shell.
Documentation uses system sans and monospace, warm neutral palettes, compact
corners, and Light/Dark/System preferences. Fumadocs supplies the page tree,
search, TOC, and navigation; native controls handle theme selection and modal focus.

## Two surfaces, shared identity

The marketing shell uses `SITE.designMode` (`terminal`) from `src/lib/site.ts`.
The documentation shell uses `data-design="reader"`. Reader tokens in
`src/styles/global.css` must not change the marketing or live-terminal palette.
Keep each reader rule in the single documentation section rather than appending
a second theme override.

- Reader prose and navigation use `-apple-system, BlinkMacSystemFont, "Inter",
  "Helvetica Neue", sans-serif`. Code uses `"SFMono-Regular", Consolas,
  "Liberation Mono", Menlo, monospace`. Inter is a local fallback, not a download.
- Reader `--font-sans` and `--font-mono` aliases include portalled search UI.
  The shared head no longer fetches IBM fonts. Paper Mono remains outside the reader.
- The wordmark is lowercase `phux`; navigation uses sentence case.
- The favicon and mark-only surfaces use `docs/assets/fox-mark.*`.
- Information hierarchy comes from type size, weight, spacing, and position.
  Color reinforces it; color never carries state alone.

## Reader tokens and typography

The reader uses off-white in light mode and warm graphite in dark mode, with
muted earth-tone accents. Tokens map to Fumadocs' `--color-fd-*` palette,
including portalled search UI. Head theme colors match the two canvas colors.

| Token (`--color-`) | Light | Dark |
|---|---|---|
| `bg` | `#faf9f6` | `#111110` |
| `bg-soft` | `#f1f0ec` | `#191918` |
| `fg` | `#242320` | `#e9e6df` |
| `dim`, `muted` | `#65615a` | `#aaa69e` |
| `subtle` | `#69645c` | `#969188` |
| `rule` | `#dedbd4` | `#33312d` |
| `surface` | `#f6f5f1` | `#151514` |
| `surface-raised` | `#eae7e0` | `#24231f` |
| `accent` (also `cyan`) | `#705b3f` | `#c6b69d` |
| `accent-soft` | `#c8bda9` | `#595044` |

The native Theme selector supports Light, Dark, and System; System is the default.
next-themes owns persistence and system updates. A head script applies the saved
preference before paint and Astro document swaps.

- `--color-bg`: page canvas; `--color-bg-soft`: code and supporting surfaces.
- `--color-fg`: primary text; `--color-muted`: supporting and navigational text.
- `--color-rule`: quiet borders; `--color-accent`: links, focus, current position.
- `--reading-width`: 72 characters for prose, without narrowing technical tables.
- Body text is 16px with 1.8 line height. The article lead is 17px; H2 is 24px.
  The page title scales from roughly 30px to 42px.
- Utilities stay at least 13px. Commands may be 13px but remain zoomable and
  horizontally scrollable. Do not shrink text to make long commands fit.
- Reader radii `sm`/`md`/`lg` are 3/5/6px; `xl`/`2xl` are 8/10px for framework
  surfaces. Overview cards, code blocks, and tables use the compact surface tokens.

## Landing and live shell

The landing has no top navigation or visible tagline. The wordmark, install
command, release links, tour, and three feature links lead to a quiet footer
containing Docs, Try, and Source navigation. Other marketing pages retain their
header. The landing canvas is `#0b0b0d`, with `#ededee` text and neutral accents.

The hero tour shows a server, clients, and signals travelling along connections.
It autoplays only while on screen, with pause and chapter controls; reduced motion
shows settled chapters. Faint background hosts are decorative, hidden from
assistive technology, and do not claim live status. Hovering or keyboard-focusing
the scene link reveals the shell invitation; touch devices show it continuously.
Activating the scene opens the live shell. `/more` retains its opt-in, pausable,
scrubbable explainer.

The live dialog uses one rounded glass shell with a blurred backdrop, not stacked
outer frames. The terminal canvas is inset within it; pane controls sit below the
canvas, using icons with accessible names and a labelled primary Split pane action.
The Info disclosure opens an inspector for runtime, network/file limits, shortcuts,
and session controls, plus identity, startup timing, and focused terminal when
available. Runtime and connection status remain in the header.
Terminal interiors use local neutral tokens;
program output and reader themes are not recolored.

## Article structure

An article has one tree-derived breadcrumb, one title, and one useful full summary.
Do not repeat the source TL;DR in the body, hide the summary behind a disclosure,
or inject another section primer ahead of the author's content. Stability and
exact-source provenance belong in a quiet footer, not a warning before every guide.
Source links remain pinned to the synchronized revision. Heading IDs and deep links
must survive presentation changes.

Prose links are underlined in the reader accent; headings remain ordinary text with
discoverable anchor links. Headings have generous separation without a rule above
every section.
Use callouts for warnings or meaningful constraints, not decorative emphasis.

## Static technical content

Fenced code stays in the original Shiki-highlighted HTML. No second React root,
shadow DOM, line-selection model, or client-side syntax renderer replaces it.
A lightweight enhancement adds a visible Copy code control and polite copy status;
clipboard failure explains how to copy manually. Code remains selectable,
searchable with browser Find, and keyboard-scrollable. Without JavaScript, the
static code remains readable and selectable.

Tables retain native table semantics inside a labelled, focusable scrolling region.
Code and table overflow stays local to its surface rather than widening the page.
Never wrap byte diagrams or clip commands to fit a viewport.

## Navigation and overview

`/overview` is the sole primary docs home. Its task cards and first-run steps
connect the reader's goal to a working path. The model figure shows a server,
sessions, a terminal resource, and client views. Quickstart and performance evidence
are primary destinations. `/docs` remains the addressable complete index.

The task tree groups Start here, Use phux, Run coding agents, Connect machines,
Performance & comparisons, Troubleshoot & maintain, Reference, and Contribute.
Keep existing public URLs when regrouping nodes. Breadcrumbs come from that tree,
including virtual folders, rather than guessed URL segments or hardcoded prefixes.
Long normative and generated pages retain their TOC and source fidelity.

Search remains a static advanced index with section hits. Exact and prefix title
matches move whole page groups forward, leaving each heading/text hit with its
parent. Failed index loading gets an announced error, a genuine retry, and an
all-documentation escape path; an error must not masquerade as zero matches.

## Responsive and interaction rules

- Use Fumadocs' actual 768px drawer breakpoint. Do not hide its desktop sidebar at
  a different CSS breakpoint while leaving the framework in desktop mode.
- Tablet uses a compact sidebar and collapsible controls; the main reading area
  and TOC remain framework-managed. Cards collapse to one column when needed.
- The mobile sidebar retains the framework's tree and triggers inside a native
  modal dialog. Escape closes it, focus starts on Close, Tab stays inside, and
  closing returns focus to the opening control. Opening search closes the drawer.
- Mobile navigation targets are at least 44px tall. Theme and copy controls have
  visible labels; decorative arrows do not enter the accessible name.
- Keep visible two-pixel focus outlines and a working skip-to-documentation link.
- Reduced-motion preference disables reader animation and smooth scrolling.
- Do not persist the entire document body between Astro routes: article, active
  navigation, TOC, copy controls, and metadata must update together.

Before landing UI changes, inspect overview, an article, and code/table-heavy
reference pages at 320px, tablet, and desktop widths in both themes. Exercise
Escape, Tab, skip navigation, copy success/failure, theme persistence, search
failure/retry, deep links, and route/back navigation on the built site.
