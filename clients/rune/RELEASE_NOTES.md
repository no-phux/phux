**Phux Rune 0.1.47**: quieter session chrome, clearer terminal navigation and reliable keyboard handoff.

- Jump back to the last pane or tab, click the shown tab to return to the previous one, and find terminals grouped by tab in the palette. (#2814)
- Option-click at a shell prompt to move its cursor. Search copy mode with `/`, `?`, `n` and `N`, and copy the selected match. (#2814)
- Show pane numbers, focus the sidebar from the keyboard, and explicitly toggle typing into every pane of the tab. (#2783)
- Choose a default session and opt out of shipped keybindings in configuration. (#2773)
- Open the palette on your terminals first, with mirrors last. Draw jump-to as a filled pill and quiet the session header.
- Show terminal dimensions only while the window resizes.
- Return keyboard input to the terminal after clicking New session. (#2759)
- Clear a copy selection with Escape before leaving copy mode, and support scroll actions. (#2755)
- Distinguish temporary connection failures from credential refusals when reconnecting. (#2763)
- Require live session and selected-terminal evidence in native iOS qualification. (#2741)
