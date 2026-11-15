**Phux Rune 0.1.30**: Rune input, scrolling and text-editor fixes since 0.1.29.

- Background updates appear immediately even when typing or a pointer action changes the same frame; the action keeps its own animation.
- Wheel, trackpad, touch and page scrolling pass over nested viewports with nothing to scroll.
- Pressing a tooltip's control dismisses its help without moving focus.
- Text editors use less copying for long drafts, undo and paste measurement; pretty wrapping handles lines containing only an inline box safely.

Tracks Rune dev `bd4d42b..1dba342`. Desktop and phone forms and the native
iOS simulator are qualified separately. Native device installation is not
desktop publication, and simulator input is not physical keyboard/IME or
real-phone feel qualification.
