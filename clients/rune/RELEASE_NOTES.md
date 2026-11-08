**Phux Rune 0.1.3**: a native iOS foundation, plus the bare terminal window.

- The phone view now runs in a native UIKit/Metal host with system text input,
  safe-area and keyboard layout, and authenticated QUIC/WSS connections.
  Enrollment uses a device-held signing key and Keychain credentials. CI
  builds and qualifies the simulator app against a paired test server.
- Native keyboard accessories target the active editor, including agent
  questions; terminal Ctrl/Alt/Shift latches work with system input. Glass
  controls keep their contrast over dark terminal backgrounds.
- The desktop's hidden bare-window chord drops into the terminal UI without
  the surrounding chrome.

The desktop packages remain the downloadable release. The native iOS work
is a simulator-qualified foundation, not a provisioned device app or
TestFlight distribution. Physical keyboard/IME, device and VoiceOver
qualification remain in progress; see `clients/rune/ios/README.md`.
