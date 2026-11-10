**Phux Rune 0.1.10** makes native iOS onboarding scan-first, with clearer Tailscale discovery and SSH recovery.

- **Scan QR, explicitly Paste, or Find with Tailscale.** Import a pairing invitation instead of transcribing endpoints, certificate pins or tokens. Review and confirm the peer before automatic platform-held key enrollment; saved-key reconnect reuses that identity.
- **Tailscale discovery is not the VPN.** Rune's embedded directory is a separate Tailscale node. The installed Tailscale VPN must be connected for the phone to reach the selected computer.
- **SSH uses your computer's local account, not your Tailscale email.** The password is used once for pairing and is never persisted. Rune no longer guesses the account from an email address. Failed attempts offer a prominent retry and QR alternative, plus credential-free local phase/error codes. Pairing does not install or upgrade phux, or start its server.

### Verification and availability

An ad-hoc 0.1.9 preview was installed and launched on a physical iPhone 18 Pro. The user's device discovery worked, but SSH login failed; that report drove the account guidance and recovery fixes. Successful user authentication, camera scanning and Secure Enclave enrollment have **not** been demonstrated on that physical device.

Simulator qualification passed 14 native groups and QUIC/WSS enrollment with same-key reconnect after relaunch. Validation also passed 19 Swift SSH tests, 82 Python driver tests, 518 Rune library tests and the Rune dependency-policy check.

This public 0.1.10 release contains desktop archives and a macOS DMG. Native iOS builds and simulator evidence are CI artifacts, not a public iPhone OTA installer or a TestFlight release.
