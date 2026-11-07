**Phux Rune 0.1.0-alpha.16**: steadier phone Find on the latest Rune.

- **Phone Find keeps focus.** Pressing the drawn keyboard or accessory bar
  no longer takes focus away from Find or sends its typing to the terminal
  behind it. Palette editing, Return, Escape and one-shot modifiers keep
  their existing behavior. The phone form is still a phone-sized desktop
  window, not an iOS or Android app.
- **Updated Rune engine.** Tracks upstream dev `53c7f60`, including
  padded-text wrapping, pixel-snapped borders, moving-text rendering fixes
  and frame timing reports in headless runs.
- **Fullscreen follows the window.** The existing macOS fullscreen action
  now targets its owning Rune window through Rune's transaction API.

Includes alpha.15's Galaxy and Reef fleet views and JPEG mirror support.
