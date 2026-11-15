**Unreleased — sidebar interaction and layout**

- Move terminal and document rows, or whole tabs, into another session by dragging onto it, including empty and collapsed sessions. Invalid or cancelled drops leave ownership unchanged.
- Close any local session with its visible **×**, with a confirmation naming the session before its processes end.
- Separate session headers from resource rows, with independent disclosure and resource counts. Selection never changes what you have folded.
- Put the machine picker and Inbox beside the traffic lights, remove the duplicate machine heading and top colour strip, and keep creation buttons stationary while their shortcut hints appear.

**Phux Rune 0.1.30**: Rune input, scrolling and text-editor fixes since 0.1.29.

- Background updates appear immediately even when typing or a pointer action changes the same frame; the action keeps its own animation.
- Wheel, trackpad, touch and page scrolling pass over nested viewports with nothing to scroll.
- Pressing a tooltip's control dismisses its help without moving focus.
- Text editors use less copying for long drafts, undo and paste measurement; pretty wrapping handles lines containing only an inline box safely.

Tracks Rune dev `bd4d42b..1dba342`. Desktop and phone forms and the native
iOS simulator are qualified separately. Native device installation is not
desktop publication, and simulator input is not physical keyboard/IME or
real-phone feel qualification.
