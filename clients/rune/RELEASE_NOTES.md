**Phux Rune 0.1.0-alpha.12**: read-only document mirrors beside your terminals, and the latest Rune.

- **Read-only mirrors.** Open live accessibility trees and PNG surfaces from
  the palette, switch their tabs, or put them beside terminals in a split.
  Trees are selectable JSON; images fit without cropping. These are native
  views, not embedded browsers: typing, paste and file drops never reach a
  document pane, and closing a view leaves its producer running.
- **Start a source.** **Start page/tree/surface mirror…** in the palette
  explains the prerequisite and starts a dedicated local producer terminal,
  never a command in your focused shell. Page capture needs Chrome debugging
  on localhost:9222 with a separate profile; tree and surface capture need
  macOS Accessibility and Screen Recording permission, respectively. Remote
  producer starts are refused rather than guessing the other machine's
  platform or permissions.
- **Live images stay live.** PNG decoding is bounded and coalesced so busy
  sources and closed views do not bury a newer frame under old work.
- **The latest Rune.** Updated text shaping and script-appropriate CJK font
  fallbacks, plus upstream text-selection and rendering corrections. Terminal
  cell geometry and instant output stay unchanged. Desktop and phone forms
  are both qualified; this is still a phone-sized desktop window, not an iOS
  or Android build.
