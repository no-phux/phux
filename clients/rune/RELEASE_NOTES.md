**Phux Rune 0.1.0-alpha.23** brings Rune's latest text and rendering fixes to desktop and the phone form.

- Text keeps its exact line height, and translucent borders paint over their backgrounds. Single-line field placeholders no longer wrap below their fields.
- Native text fields offer their own Cut, Copy and Paste menu. Connected-runtime checks prove Find's Copy and Cut cannot address the terminal behind it, on desktop or phone.
- Notifications now use Rune's standalone notification crate; macOS behavior and Linux notify-send delivery are unchanged.

This tracks Rune dev `5bcb541`. The new held-modifier observer is not adopted: Rune's ordinary native window host drops its callback, so Phux retains its existing working route.

Still an alpha. Phone support remains a desktop-hosted form, not an iOS or Android app. Startup motion diagnostics still include asynchronous title/viewport changes; no native mobile or complete motion qualification is claimed.
