**Phux Rune 0.1.0-alpha.19**: spatial phone navigation and focused Settings.

- **Move through your actual layout.** In `--phone`, a spring camera follows
  the focused terminal through the tab's real split arrangement. The compact
  top-right minimap tracks your position: tap a region, swipe the map or Move
  handle, or open Panes to choose by name. Saved split order and ratios stay
  unchanged; other panes keep their views, history and selection.
- **Scroll without changing panes.** A separate Scroll handle follows the
  terminal's history or application-aware arrows/mouse-wheel policy. The
  navigation toolbar has its own space outside the terminal grid, and the
  minimap lives in the header: neither covers text or the last-row prompt.
- **Settings, not a wall of themes.** Grouped categories, current values,
  cross-category search and a restrained preview replace the old page.
  Desktop has a scrollable category rail; phone has readable, thumb-sized
  drill-down rows and bottom search. Theme selection opens the existing live
  preview palette and returns to Settings on accept or cancel.
- **A keyboard just for Settings search.** On phone, focusing search shows
  its keyboard even if you hid the terminal's. Done and Hide preserve the
  query without changing that terminal preference. Native Find targets
  Settings search, and accessory commands respect the active modal.
- **Mirrors recover without stealing focus.** Delayed replacement documents
  restore by stable identity across organization and reconnect gaps, unless
  you choose another pane. Batched closures cannot reopen a dead sibling;
  immediate replacements no longer flash an unavailable error.

The phone form remains a desktop-hosted stand-in, not a native iOS app.
