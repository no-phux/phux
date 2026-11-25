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
names. Every bundle carries `PrivacyInfo.xcprivacy` at its root and declares
`ITSAppUsesNonExemptEncryption` false (only the OS's TLS/QUIC), so a store
upload needs no per-build export-compliance answer.
