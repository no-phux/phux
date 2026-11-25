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
Click a pane thumbnail or exposed surface to change the in-memory focus.

- **A / Inline — selected:** smaller, almost-flat rows. A split row is 38px
  rather than 55px, with a 28 × 17px mark rather than 50 × 30px. Only faint
  1px/2px layered edges suggest depth; no shadow, tilt or exposed deck. Keep
  readable labels, visible attention and plain singles instead of shrinking
  every element. This captures the user's request for depth discovered only
  on close inspection, not a loud 3D treatment.
- **B / Exposed:** selected work becomes a labelled dimensional deck; other
  groups stay single lines.
- **C / Spatial:** pane labels and focus live inside a shallow stacked thumbnail
  of their actual split.

All use the same seven illustrative groups and twelve panes. Grouping represents
tab/split membership, not agent-process parentage. Geometry is illustrative,
fixed fixture data; it is not a server layout implementation.

This is a browser composition study beside the actual native sidebar module.
It is not imported by Rune, does not connect to a server, run commands, persist
state, or replace the production UI. Native token mappings, arbitrary layouts,
session actions, drag ownership and accessibility require proper implementation
after a direction is selected. CSS depth offsets are proposals, not new
production tokens. Keep this study on its throwaway branch, outside `main`.
