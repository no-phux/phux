**Phux Rune 0.1.33**: long text fields stay accessible.

- Track Rune dev `0796df5f` from `bc05f817`, including both native
  accessibility adapters. On macOS, text fields above 64 KiB, or with paragraphs
  still waiting to be shaped, retain their complete value and selection
  ranges instead of exposing an absent or partial text document. Range queries
  are answered lazily; lines follow newline boundaries and have no rectangles.
- Accessibility text runs no longer copy the entire field once per line.
  Normal password masking and short-field IME display handling remain intact.
- Inherit Rune's paragraph-field highlight/reveal fixes. Terminal Find still
  searches the loaded terminal grid; this does not add Find inside ACP editors.

Known upstream limitation (`phux-eqdvh`): in the new macOS fallback, an
AppKit UTF-16 query inside an emoji's surrogate pair rounds forward to the
following character, which can also shift selection/edit boundaries.
This also affects partially shaped fields below 64 KiB.
Fully shaped normal fields keep their existing run-backed path. The finding is
tracked, with a reproduction retained; no local framework fork is included.

This does not add native iOS VoiceOver support or prove physical-device feel.
Signed phone installation is reconciled separately from desktop publication;
an unsigned device build or simulator screenshot is not phone delivery.
