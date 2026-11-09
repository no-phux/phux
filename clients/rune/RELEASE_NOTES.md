**Phux Rune 0.1.7**: the Kitchen view and the newest Rune.

- **Open Kitchen** from the palette shows every agent as a chef on its
  machine's line: working chefs cook, one that needs you rings the bell at
  the pass, finished ones plate up and idle ones take a break. Lines keep a
  fixed order, so they never swap when an agent starts needing you. Press a
  chef to select it; press it again or Return to show its terminal.
- Text fields (the palette, find, rename, Settings search and the inbox
  reply) undo the way a browser's do: a run of typing, line breaks and
  deletions is one step, and moving the caret or changing the selection
  starts the next.
- Tab with nothing focused continues from the control you last used rather
  than the start of the window.
- Built on Rune `a8a5351`. Desktop and desktop-hosted phone layouts draw as
  they did in 0.1.6.

The downloadable release is still the desktop client (macOS arm64 and Linux
x86_64/arm64). Native iOS remains a simulator-qualified build with signed
ad-hoc packaging; it is not physical-device, keyboard/IME, VoiceOver or
TestFlight qualification. The embedded late-frame presentation limitation
documented in 0.1.5 is unchanged.
