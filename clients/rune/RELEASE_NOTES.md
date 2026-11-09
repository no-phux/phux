**Phux Rune 0.1.6**: direct iOS preview packaging and steadier qualification.

- Native iOS developers can package a device build into a signed ad-hoc IPA
  and HTTPS install page without TestFlight. Packaging checks the exact
  bundle, distribution certificate, expiry and every requested device before
  signing. The private signing key stays in the local keychain.
- Native simulator qualification now records setup timings, keeps diagnostic
  output separate, validates its retained evidence and reuses the verified
  app container instead of looking it up again after launch. Cold container
  acquisition remains bounded; input, authentication and lifecycle checks
  are still required.
- In the desktop-hosted phone form, tap a terminal link to open it; dragging
  still selects text. Native iOS URL-opening qualification remains separate.

The downloadable release is still the desktop client (macOS arm64 and Linux
x86_64/arm64). iOS installation requires an Apple profile covering the actual
phone and the user's approval. Packaging is not physical-device, keyboard/IME,
VoiceOver or TestFlight qualification. The embedded late-frame presentation
limitation documented in 0.1.5 is unchanged.
