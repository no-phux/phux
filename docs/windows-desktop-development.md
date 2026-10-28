# Windows desktop development

The CLI and server run in WSL 2, with PowerShell launchers. A native Windows
desktop app is available from Desktop 0.50.0 onward.

The Windows desktop is a separate native MSVC build of the
[GPUI workspace](./development.md#gpui-desktop), with Direct3D 11/DirectWrite
rather than Metal or Vulkan. Its coordinator and CLI remain in WSL 2. Set
`PHUX_WSL_DISTRO` to select a distribution; `--phux` is a Linux executable
path and `--socket` a Linux socket path.

A development Windows app requires an explicit isolated socket and does
not discover the installed server by default. Windows-native qualification
must exercise the packaged `.exe` and real window input, not the Linux TUI
or the macOS/Linux headless renderer fixtures.

These notes are for testing development builds; released packages use the
[desktop installer](./install.md#desktop-app). `Ctrl-Shift-P/T/W/F` are
palette, new terminal, close pane, and find; `Ctrl-Shift-C/V` copy and paste;
`Ctrl-Shift-D` and `Ctrl-Alt-D` split; `Ctrl-Shift-R` reconnects. Plain
`Ctrl-C/Z/D` stay with the shell. Closing the app detaches; the WSL processes
keep running. The local Changes panel and the external Unix developer
socket are not available for WSL checkouts. Use Git in the terminal and the
in-app Performance Monitor instead. The build logs to
`%LOCALAPPDATA%\phux-desktop\logs\desktop.log`.
