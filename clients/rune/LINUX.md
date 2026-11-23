# Linux qualification and input methods

See [Install](README.md#install) for archives and system requirements.

**Wayland is checked on one compositor.** Every build is opened on a headless
sway and used there: typing (non-ASCII included), paste and copy through the
Wayland clipboard, Chinese composed through an input method (fcitx5 pinyin),
and fractional scaling (1.25 and 1.5). Not checked: other input methods and
dead keys, GNOME and KDE. There is no middle-click paste: a selection does not
set the primary selection. The clipboard uses the data-control
protocol that wlroots compositors, Hyprland and KDE offer. On a compositor
with XWayland, unsetting `WAYLAND_DISPLAY` for the app runs it on X11.

## Upstream IME cursor synchronization

All ten Rune pins, including `winit`, use the same upstream revision.
Upstream [#1454](https://github.com/anomalyco/rune/pull/1454) (`263b799`) supplies
`ImeSync` to suppress unchanged cursor-area calls while retaining movement,
scaling, reset and input-context reentry. The temporary winit fork is retired.
Real sway/fcitx5 qualification remains mandatory: check candidate positioning
and composition through movement, resizing and fractional scaling, including
focus and input-context changes. Source review and the
[upstream regression tests](DEVELOP.md) do not establish real IME behavior,
GNOME/KDE coverage or packaged/native qualification.
