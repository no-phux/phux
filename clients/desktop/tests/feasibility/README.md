# Solid / native terminal feasibility

**TL;DR.** This is the integrated production-bundle fixture, not product UI.
Its acceptance path is two Solid `<phux-terminal>` surfaces, one native client,
one owned Bash PTY, real Vim input, Metal captures, and explicit teardown.
It needs source-built GPUIX with patches 3 and 4 (custom-element admission and
close-before-exit); no passing run is recorded in-tree.

## Run

From the repository root, after installing the pinned desktop dependencies:

```sh
PHUX_DESKTOP_ADDON=/absolute/path/to/phux-desktop-native.darwin-arm64.node \
  bash clients/desktop/tests/feasibility/run.sh
```

The Rust harness uses `phux-server-testkit` in its own `.cache` Cargo workspace
and target directory. It creates a temporary home, XDG directories, socket,
Bash PTY, and editor file. Neither the user's daemon nor shell configuration
participates. The standalone harness's `nix` dependency supplies checked Unix
process-group signaling: even a failed Bun runner cannot leave its native
window descendant alive. Bun owns and reaps the window child; Rust owns the
runner process group, reaps the runner, then shuts down and joins the server.

## Acceptance path

`scripts/check-terminal.ts` uses the parent-owned `scripts/desktop-bundle.ts`
helper to verify the source bootstrap, rebuild the patched framework JS, and
build `entry.ts` with the production Solid plugin/source resolver. The entry
loads the canonical combined addon through
`native/loader.mjs` **before** dynamically importing the Solid window. The
window uses the production renderer with `focus: false`, never a test renderer.

The initial connection targets the seeded `solid-feasibility` session. The
sole native notification callback drains once per wake, discovers the
attached terminal only after its replica is input-ready, updates metadata and
the Solid `paintRevision` signal, and ignores queued wakes after close. Cells
and glyphs never enter JavaScript. Native search results are bounded match
handles used as output synchronization; they are not proof of painted pixels.

The runner exercises these stages:

1. Mount two native surfaces with distinct view IDs for one terminal.
2. Commit a Bash command through `commitText` and normalized Enter. Delayed
   output ensures the redraw happens after the toolbar event; no app timer or
   polling loop drives paint. Output contains CJK, combining text, and emoji.
3. Scroll only the left view and select only the right through the declared
   binding. Check distinct offsets, exact selected text, and no left selection.
4. Open real Vim on the owned file, wait for its initial content, commit UTF-8
   editing input, then send Escape and `:wq`. Verify exact saved UTF-8 bytes.
5. Remove both Solid surfaces, destroy both native views, close the client,
   process its final event batch with the same event consumer, and verify zero
   remaining terminal elements. Solid cleanup repeats teardown idempotently.

Successful runs produce `.cache/terminal-feasibility/{shell,independent-views,
editor,closed}.png` plus `stderr.log`, whose receipt contains native view IDs,
notification revisions, ordered events, and the final close batch. Inspect
the PNGs for actual terminal output; metadata and retained-tree assertions
alone do not establish Metal glyph fidelity. Resize negotiation, native
keyboard/IME routing, and pixel-fidelity acceptance belong to their respective
gates and are not claimed by this toolbar-driven fixture.
