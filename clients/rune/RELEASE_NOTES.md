**Phux Rune 0.1.24**: 6 changes since 0.1.22.

- Track Rune dev `65f8eb4`: keep terminal holds, pane navigation, arrow-pad
  drags and live mirrors under the app's control. Cancelled mirror gestures
  no longer click the host. Preserve the pane-map springs and name five
  search/reply fields for accessibility (#2474).
- Offer the pane in front to your iPhone through Handoff (#2473).
- Refine phone Sessions and frost the floating navigation dock (#2458).
- Honor Option as Meta without affecting native text fields (#2442).
- Wait for the quick terminal's move receipt before changing its window (#2447).
- Open `phux://connect` links to pair and attach (#2432).

Rune also reuses unchanged paint, grows smaller initial image/glyph atlases,
uses more precise text widths and preserves macOS Metal sRGB tags.

Native iOS simulator input and paired QUIC/WSS reconnects are qualified;
this desktop release is not a new phone installation or physical-device
keyboard, IME, accessibility or feel qualification.
