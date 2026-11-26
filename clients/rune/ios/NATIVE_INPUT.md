# Native input

UIKit implements `UITextInput`; Rune paints the terminal and fields. Terminal
capture keeps marked text/dictation local until committed. Paste retains the
existing confirmation boundary. App fields are edited synchronously through
their app-owned `TextEdit`, not an unaddressed deferred IME event. Snapshots
carry target identity, revision and UTF-16 selection; stale owners and split
surrogates fail closed. Unknown/inert fields never fall through to terminal
input. Return uses the same palette/search/rename/inbox/question handlers.
The terminal accessory's one-shot Ctrl/Alt/Shift latches apply to system
keyboard input too. Native fields use their system keyboard modifiers;
their accessory offers Done instead of terminal latches. Tap the field to
resume editing after Done. Accessory keys never type through a focused or
held ACP editor into the terminal behind it. The native accessory is laid
out after every sheet's modal content, so its controls remain tappable;
unmounted ACP fields lose only focus/preedit, never their committed draft.

Native menu actions use a separate [request-bound callback](./NATIVE_MENUS.md),
not the keyboard's target token.
