**Phux Rune 0.1.49**: return keyboard focus to the terminal after clicking tabs and toolbar controls.

- Keep typing with the focused control instead of splitting it between the desktop controls and the terminal. Escape gives focus back to the terminal. (#2851)
- Update the `url` dependency to address RUSTSEC-2026-0592. (#2848)

Also includes improvements prepared for 0.1.47 and 0.1.48, which were not published.

- Keep waiting-agent marks crisp in forced colours, without restoring grey halos. Status words, marks and keyboard focus remain; ordinary shadows follow Rune Kit's CSS layer order.
- Polish phone navigation and native glass controls. Keep Settings' keyboard dismissal and accessory keys readable at large text sizes: Done stays whole, keys fit their labels, and an overflow indicator makes the remaining keys discoverable.
- Jump back to the last pane or tab, click the shown tab to return to the previous one, and find terminals grouped by tab in the palette. (#2814)
- Option-click at a shell prompt to move its cursor. Search copy mode with `/`, `?`, `n` and `N`, and copy the selected match. (#2814)
- Show pane numbers, focus the sidebar from the keyboard, and explicitly toggle typing into every pane of the tab. (#2783)
- Choose a default session and opt out of shipped keybindings in configuration. (#2773)
- Explain when an action needs a newer server and offer a local server upgrade. (#2815)
- Open the palette on your terminals first, with mirrors last. Draw jump-to as a filled pill and quiet the session header.
- Show terminal dimensions only while the window resizes.
- Return keyboard input to the terminal after clicking New session. (#2759)
- Clear a copy selection with Escape before leaving copy mode, and support scroll actions. (#2755)
- Distinguish temporary connection failures from credential refusals when reconnecting. (#2763)
- Require live session and selected-terminal evidence in native iOS qualification. (#2741)
- Avoid redundant Wayland caret-rectangle updates while preserving layout and DPI changes. Keep real input-method qualification strict and retain focus, engine and protocol diagnostics on failure; this does not claim that every intermittent input-method failure is repaired.
- Preserve explicitly unvalidated native iOS failure evidence before simulator deletion, without relaxing input checks, deadlines or acceptance.
