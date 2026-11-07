**Phux Rune 0.1.0-alpha.14**: readable text-field carets and safer Find clipboard shortcuts.

- **Text fields follow the theme.** The caret and selection use the active
  theme's foreground in both desktop and phone forms, including when you
  switch between light and dark themes.
- **Find keeps clipboard actions out of the shell.** Terminal Copy, Paste
  and Select All shortcuts cannot act on the terminal behind a focused Find
  field. A clipboard reply requested before Find took focus is refused too.
  Native field editing uses ⌘C/V/A on macOS and plain Ctrl+C/V/A on Linux.
- **Linux notifications.** With `notify-send` available, the app notifies
  you when an unseen terminal needs attention; clicking switches to it.
  Notifications withdraw through `gdbus` when answered or seen. On Hyprland,
  clicking also raises the window; other desktops may leave it behind.
- **Pane names arrive sooner.** With a current server, program titles appear
  in pane lists before their terminal streams start. User-set titles still win.
- **The latest Rune.** Updated to upstream dev `1ad9cf6`, including native
  text-field editing and non-Latin shortcut corrections. The phone form
  remains a phone-sized desktop window, not an iOS or Android build.
