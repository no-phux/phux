---
audience: contributors
stability: stable
last-reviewed: 2026-09-30
---
# phux site design system

**TL;DR.** Marketing keeps its terminal identity. Documentation is a separate,
scoped reading surface: proportional typography, restrained blue, light/dark/system
preferences, static technical content, and task-based navigation. Reuse Fumadocs
for the page tree, search, TOC, and navigation; use native browser controls for
copying, theme selection, and modal focus behavior.

## Two surfaces, shared identity

The marketing shell uses `SITE.designMode` (`terminal`) from `src/lib/site.ts`.
The documentation shell uses `data-design="reader"`. Reader tokens in
`src/styles/global.css` must not change the marketing or live-terminal palette.
Keep each reader rule in the single documentation section rather than appending
a second theme override.

- IBM Plex Sans is the reading and navigation face. IBM Plex Mono is for commands,
  selectors, code, and wire structure, not ordinary chrome.
- The wordmark is lowercase `phux`; navigation uses sentence case.
- The favicon and mark-only surfaces use `docs/assets/fox-mark.*`.
- Information hierarchy comes from type size, weight, spacing, and position.
  Color reinforces it; color never carries state alone.

## Reader tokens and typography

The light reader uses a white canvas, dark slate text, and blue `#245dcc` links.
The dark reader uses a `#10151d` canvas, soft slate text, and blue `#8ab4ff` links.
Both map the same semantic tokens to Fumadocs' `--color-fd-*` palette, including
portalled search UI. The native Theme selector supports Light, Dark, and System;
next-themes owns persistence and system updates. A small head script applies the
saved preference before paint and Astro document swaps.

- `--color-bg`: page canvas; `--color-bg-soft`: code and supporting surfaces.
- `--color-fg`: primary text; `--color-muted`: supporting and navigational text.
- `--color-rule`: quiet borders; `--color-accent`: links, focus, current position.
- `--reading-width`: 72 characters for prose, without narrowing technical tables.
- Body text is 16px with 1.8 line height. The article lead is 17px; H2 is 24px.
  The page title scales from roughly 30px to 42px.
- Utilities stay at least 13px. Commands may be 13px but remain zoomable and
  horizontally scrollable. Do not shrink text to make long commands fit.
- Reader surface radii are 6–14px. Task cards group choices on the overview;
  ordinary articles do not need cards around every paragraph.

Marketing retains its flat, square-edged rules, terminal palette, and live-island
presentation. Reader tokens do not restyle those surfaces.

## Article structure

An article has one tree-derived breadcrumb, one title, and one useful full summary.
Do not repeat the source TL;DR in the body, hide the summary behind a disclosure,
or inject another section primer ahead of the author's content. Stability and
exact-source provenance belong in a quiet footer, not a warning before every guide.
Source links remain pinned to the synchronized revision. Heading IDs and deep links
must survive presentation changes.

Prose links are underlined and blue; headings remain ordinary text with discoverable
anchor links. Headings have generous separation without a rule above every section.
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

`/overview` is the sole primary docs home. Its task cards and explanatory figure
connect the reader's goal to a working path; the figure shows a person, an app,
and an agent sharing one running terminal. Quickstart and performance evidence
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
