**Phux Rune 0.1.11** makes the desktop-hosted phone keyboard work in sheets and keeps phone mode when switching machines.

- **Type into phone sheets.** The drawn keyboard now edits rename fields, machine setup and typed inbox replies in the desktop-hosted phone form. Sheets leave room for the keyboard and accessory row. Return, Escape, selection replacement, delete and one-shot shift stay with the active field; stale or missing inbox reply editors do not accept text.
- **Switch machines without losing phone mode.** The phone form and keyboard/symbol preferences survive machine replacement, while old pane navigation, picker and modifier latches are cleared.
- **Native editing is unchanged.** Native iOS fields still use UIKit. Drawn-keyboard editing of ACP drafts remains unsupported; native ACP keeps its existing mounted-editor path.

Rune stays on the newest-good framework revision, `9c28a20`. The newer dev revision `45eb5cab` still contains the regression that repeats GIFs without a loop extension forever, so this release does not adopt it.

### Verification and availability

Touch tests cover rename, machine setup, inbox replies and actual machine replacement at phone sizes, including 320×568. Desktop and phone baseline frames are unchanged outside the intended field editing. Rune's format, strict clippy, 524 Rust tests, 82 Python driver tests, four Bun tests and dependency-policy check passed for the feature.

Native simulator qualification passed 14 groups with QUIC and WSS enrollment and same-key reconnect after relaunch. This is simulator evidence, not new physical-device or TestFlight proof. Successful user authentication, camera scanning and Secure Enclave enrollment remain unverified on the physical iPhone.

This public 0.1.11 release contains desktop archives and a macOS DMG. Native iOS builds and simulator evidence are CI artifacts, not a public iPhone OTA installer or a TestFlight release.
