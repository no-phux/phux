**Phux Rune 0.1.32** keeps window colors steady as the GPU takes over and
includes Rune's macOS accessibility crash fix.

- Translucent fills and grayscale text edges use byte-precision blend sources
  in the default window pass, reducing CPU/GPU handover drift on desktop
  and native iOS without changing layout or terminal input.
- The macOS accessibility adapter no longer races or aborts when AppKit
  answers the process's own accessibility query on a background thread.
- Tracks Rune dev `bc05f817`. All five upstream workspace patches move with
  the framework; Linux and macOS archives retain their usual verification.

Native iOS continues to qualify paired QUIC and WSS, saved-key reconnect,
UIKit editing and real OS background/foreground rendering in the simulator.
Simulator and device linking are not physical-device feel, VoiceOver or
OS keyboard/IME qualification. Scheduled ad-hoc phone installation remains
separate from this desktop release and requires valid local signing access.
