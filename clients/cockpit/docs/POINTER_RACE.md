# Pointer Race

## Decision

Army C keeps Army A's retained, identity-keyed terminal interaction widget and
Ghostty `SelectionGesture`, then brings in Army B's broad libghostty mouse
encoder coverage, frame scale, horizontal wheel support, and adversarial
protocol cases. Capture and lifecycle ownership are replaced rather than taken
unchanged from either branch.

The result routes every pointer edge from native-sdk's authoritative
`canvas_widget_pointer` event. A temporary fork adds an explicit context-menu
policy: `.disabled` bypasses menu handling and keeps button 1 on ordinary
routing for a live mouse-reporting TUI; `.automatic` gives local and ended
terminals exclusively to Cockpit's declared native menu. No raw pointer echo is
applied to terminal input.

## Army A: Overlay

Useful evidence retained from commit `4f5666e`:

- Transparent retained interaction widgets are globally keyed by stable
  `ResourceId`, and their laid-out bounds agree with the independently painted
  terminal frames.
- Primary selection uses libghostty-vt `SelectionGesture` for cell, word, and
  line behavior. Pointer selection remains separate from Cockpit's keyboard
  selection mode, uses the same emulator selection for painting/copy, and is
  cleared by ordinary terminal typing.
- The overlay isolates tab chrome, split headers/divider, gutters, and the
  parked or visible WebKit surface.

Review findings not retained:

- It forwarded the raw event through UiApp, then manually routed and applied
  the same event a second time.
- It held one model-wide pointer drag, did not key ownership by window, and
  dropped captures without consistently releasing a live TUI generation.
- It covered a narrower protocol/wheel surface, did not carry frame scale into
  SGR-Pixels, and advertised the transparent panel as a textbox without an SDK
  text-value contract.

## Army B: Host

Useful evidence retained from commit `506592c`:

- libghostty's encoder supplies X10, UTF-8, SGR, URXVT, and SGR-Pixels across
  modes 9, 1000, 1002, and 1003, including modifiers and motion dedupe.
- Frame scale is required for SGR-Pixels, and wheel reporting needs both axes,
  bounded work, and transition-local accumulators.
- Its adversarial cases covered resized split mapping, Shift selection bypass,
  chrome/divider/Web isolation, reorder, restart, close, and stale generations.

Review findings not retained:

- Host routing recomputed terminal hits from app geometry instead of consuming
  native-sdk's routed widget event.
- One pointer lease was tied to placement/focus checks, so focus or attachment
  movement could revoke identity ownership and unrelated pointers could not be
  represented independently.
- Local drag selection used a hand-built cell range instead of Ghostty's
  single/double/triple selection gesture.

## Army C: Hybrid

- `CockpitHost` consumes `canvas_widget_pointer`, including the SDK target,
  click count, and captured widget ID. Terminal events return before UiApp's
  typed `on_press` dispatch. The context-menu policy makes button 1 follow that
  same routed path while a live TUI owns mouse reporting; every raw pointer echo
  is ignored for terminal input.
- A bounded capture table is keyed by `window_id + pointer_id` and stores
  `ResourceId + session_generation`. Different pointers coexist; stale edges
  cannot cancel or borrow another capture. Reorder and focus changes do not
  change ownership.
- Web/terminal hiding, detach, close, restart, deactivation, cancel, a second
  down for the same physical pointer, process exit, and generation mismatch all
  retire a capture once. A reporting capture emits a release only while its
  exact active generation and mouse-protocol fingerprint still accept input.
  Motion, wheel, and hover reports require the same generation, visibility, and
  `acceptsInput` gate.
- The current frame's finite positive scale is model state. SGR-Pixels converts
  pane-local canvas points to backing pixels; tests pin scale 1, 1.5, and 2.
- libghostty owns format/mode encoding. Cockpit supplies vertical and horizontal
  wheel buttons, modifier state, active-button state, and bounded coordinates.
  NaN/infinite values are rejected; finite hostile deltas are clamped to a
  64-report total event budget, fairly interleaved across axes, without making
  either accumulator non-finite. Local scrollback keeps a separate remainder.
- Mouse mode/format fingerprints reset motion dedupe and reporting-wheel
  accumulators on every observed transition, including disable and re-enable;
  a transition also retires captures minted under the old protocol.
- Ghostty's gesture owns edge-drag autoscroll. One 15 ms host timer ticks every
  active local capture by exactly one row and stops when no gesture requests it.
- Every pane uses one transparent unbound `.terminal` interaction widget,
  supplying stable identity, the native I-beam, a textbox name, and the real
  viewport as `text_value` in every mode. The app-owned Ghostty painter remains
  the sole pixel and selection owner.
- Right/control-click presents native pane-addressed Copy/Paste actions only
  under local ownership. While a live TUI reports mouse input, the native menu
  is intentionally absent and secondary down/up go exclusively to the child.
  Shift local selection remains copyable through Cmd+C in that mode, and copy
  keeps the range highlighted after clipboard confirmation.

## SDK Seam

The temporary `phall1/native` pin at `992f9f5` extends native-sdk v0.7.1 with
`ElementOptions.context_menu_policy`. The seam is upstream-ready and deliberately
generic: `.automatic` preserves existing behavior, `.declared_only` suppresses
SDK defaults, and `.disabled` bypasses menu handling while preserving ordinary
pointer routing, capture, cursor, and semantics. Secondary ownership is decided
on down and retained through matching up/cancel even if a rebuild changes the
policy, preventing menu takeover while an ordinary gesture is in flight. An
unbound terminal still cannot bind an app-defined continuous pointer message,
and the SDK capture register is still one
`canvas_widget_pressed_id` per canvas, not per `(window, pointer)`.

The remaining adapter is deliberately narrow:

1. Keep an inert `on_press` on one transparent unbound `.terminal` so the SDK
   establishes its normal retained-widget press/capture target.
2. Intercept the SDK's already-routed `canvas_widget_pointer` event in
   `CockpitHost`; never route the raw echo into terminal input.
3. Resolve active motion/up/cancel through Cockpit's identity/generation capture
   table. If v0.7.1's canvas-global captured target belongs to another pointer,
   perform only an ID lookup for the owner's current retained bounds, never a
   coordinate hit test.
4. Set context-menu policy `.disabled` only while the target's live generation
   has mouse reporting active; otherwise use `.automatic` with the declared
   pane-addressed Copy/Paste menu.
5. Do not forward a consumed terminal widget event into UiApp's `on_press`
   handler. Accessibility activation still uses that handler normally.

This adapter can disappear when the SDK offers custom continuous-pointer
handlers plus per-window/per-pointer retained capture. Until then, removing it
would either duplicate routing or make multi-pointer generation ownership
untruthful.

## Verification

The null-platform suite covers routed single application, Ghostty cell/word/
line selection, selection-copy-type, protocols and modes, modifiers, both wheel
axes and fairness, scaled pixels, transition reset and capture fencing, hostile
numeric input, independent pointers, autoscroll, native context actions,
mode-exclusive secondary ownership, retained Shift selection, stable terminal
kind/I-beam/text-value semantics, persistent copy selection, all lifecycle
fences, strict surface isolation, and stale post-close query output. The default
run passes 142 runnable tests with four expected screenshot skips;
`COCKPIT_SHOTS=1` passes all 146 tests. The four
inspected PNGs retain the one-terminal, tabs, split, and four-terminal product
proofs without overlay artifacts. `zig build` separately passes and links the
ReleaseFast native macOS executable.

## Residual Limitations

- Native SDK v0.7.1's canvas-global pressed-widget register cannot itself
  represent concurrent pointers; Cockpit's bounded eight-capture adapter is the
  authority after each pointer's routed down.
- Protocol transitions are observed at emulator output-batch boundaries. A
  child that disables and re-enables the identical mode and format inside one
  indivisible output batch leaves the same final fingerprint, so no consumer of
  terminal flags alone can observe that transient transition.
- Deterministic screenshots cover the retained native canvas, not WebKit
  pixels, and do not replace a live AppKit/Metal pointer exercise.
