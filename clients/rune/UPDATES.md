# Updates and release channels

Open **Settings** with the sliders button at the right of the desktop title
bar, even with the sidebar folded. The keyboard shortcut is ⌘, on macOS or
Ctrl-Shift-, on Linux; **Settings** in the command palette works too. Choose
**About & Updates**, or search Settings for **channel**.

## The local phux installation

**Update phux with Phux Rune** uses the local CLI shown in Settings: the same
one Rune found at launch, not a second, private installation. **Update server**
runs that CLI's updater and hands the live local server off without closing
sessions. A refused handoff offers **Retry server handoff**, without installing
again. Even if the window shows a remote machine, this updates this computer's
phux, not the remote server.

**phux release channel** offers:

- **Follow installed** (the default): keep the CLI's existing channel choice.
- **Stable**: numbered releases.
- **Next**: the latest green main build, ahead of numbered releases.

Selecting a channel remembers it and checks for updates; nothing installs
until you click the update action. Checking and installing use the same
channel. The CLI records its channel when an update installs, so subsequent
`phux update` calls follow it too. Selecting **Follow installed** removes
Rune's override; it does not reset the CLI to Stable.

Package-managed installs show their own update command and keep their own
channel policy. Rune never replaces them through its own installer. Finish
an in-progress check, staged app update, or pending server handoff before
changing management or channels.

Development builds, and windows started with `--socket`, `--phux`, `PHUX_BIN`
or `PHUX_PROFILE`, never modify phux. If the CLI is missing, follow the
[installation instructions](./README.md#install); Rune does not silently
bootstrap another copy.

## The Rune app

Phux Rune itself has one independent app feed, not Stable/Next app channels.
**Check automatically** checks at launch and every six hours. A corner card
that never takes the keyboard offers the update.

**Download update** fetches the archive and refuses it unless a release key
signed its checksum and the new build can draw a frame on this computer. The
app then installs when you quit, or now with **Restart now**, which reopens
your session. Updates need a copy in a writable place such as Applications.

On Linux the files are replaced as soon as the download is verified, so the
next launch is the new version however you quit, even if a server update is
still due. On macOS, while a server update is due, quitting leaves the app as
it was and drops the download; **Restart & update server** moves both.

The startup check draws off-screen: a build that fails only when opening a
real window can still install. Update outcomes go to `rune-update.log`; see
[Diagnostics](./README.md#what-it-does) for log locations and **Copy diagnostics**.
