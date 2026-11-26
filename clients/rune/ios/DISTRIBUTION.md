# Building the iOS host under another identity

The defaults are the preview's own identity (`sh.phux.rune.ios`, "Phux Rune",
`phux-rune://`, the Rune crate version). A distributor ships this host as its
own app by passing its identity; phux-mobile builds its App Store app this way:

```sh
python3 clients/rune/ios/build.py --target device --release --out /absolute/scratch/ios-device \
  --bundle-id dev.phux.mobile --display-name phux --url-scheme phux --url-scheme phux-rune \
  --version 0.5.0 --build-number 812
```

The app accepts pairing links on exactly the schemes its bundle declares.
`qualify.py` qualifies whichever bundle identifier the app's `Info.plist`
names. Every bundle carries its privacy manifest (`Tailnet/PrivacyInfo.xcprivacy`, copied to the root) and declares
`ITSAppUsesNonExemptEncryption` false (only the OS's TLS/QUIC), so a store
upload needs no per-build export-compliance answer.

## Widgets and the Lock Screen card

`--app-group group.dev.phux.mobile` also builds `PlugIns/PhuxWidgets.appex`
(`widgets.py`): the Live Activity (the Lock Screen card and the Dynamic
Island) and the agents widget. The extension is the bundle identifier plus
`.agentstatus`; sign it with its own profile, before the app, both with that
App Group. Without the option there is no extension and no card.

The app shows what Rune's `Presence` last said (`src/desk/presence.rs`):
counts, then up to three panes by name, agent and session. An agent's
question never leaves the app. A card starts while the app is open and an
agent waits or works, is brought up to date as the app leaves the foreground,
and ends when all is quiet. Nothing updates it while the app is suspended, so
every surface says "as of" when it was taken, and after 15 minutes the fox
shuts its eyes. Updates while closed need push (phux-cf5ww.6).

A tap opens `phux://pane/@N` or `phux://inbox` in the app itself.
