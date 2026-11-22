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

## Temporary winit compatibility pin

The compatibility pin is a single Wayland cursor-publication patch over the
same upstream revision as the other Rune dependencies
([reviewed diff](https://github.com/no-phux/rune/pull/5)). It suppresses unchanged
caret transactions while retaining movement, scaling and input-context resets;
it changes neither incoming composition nor frame scheduling. The upstream
Windows fixes remain intact. Reunify the pins when upstream has equivalent
behavior; the exception never replaces packaged/native qualification.
