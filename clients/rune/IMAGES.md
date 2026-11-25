# Images, files and screenshots

## Pasting and dropping

Paste an image and it uploads to the server and types its path, for agents
that read images. Files pasted or dropped on a pane type as quoted paths.

## Attaching from a phone

On iOS the paperclip in the key bar (or ⇧⌘O on a hardware keyboard) opens
Screenshots, Photo Library, Take Photo and Choose File. Up to ten picks upload
to the pane's host and their paths are typed at the prompt, in order. Paste
does the same with a copied image. The system's pickers hand over only what
was picked, so nothing asks for the photo library.

HEIC and other images agents can't read become JPEG; everything else goes byte
for byte, up to 48 MiB each. Picks go to the pane that had the keyboard when
the menu opened, and are refused aloud if that pane closed or the phone moved
to another computer. A photo keeps the location the picker says it includes.

## Screenshot to Terminal

**Screenshot to Terminal** (⇧⌘O, Ctrl+Shift+O on Linux) is in the command
palette, the Terminal menu and a pane's right-click menu. It opens the
system's region picker and sends the shot the way a pasted image goes:
uploaded to the terminal's host, with its path typed at the prompt, for the
agent in that pane to read. Bind it as `screenshot` in `[rune.keys]`.

### Pickers

- **macOS** uses the system picker; Space switches to picking a window. macOS
  asks for Screen Recording the first time.
- **Linux** uses `slurp` and `grim` on Wayland, `maim` on X11, or
  `gnome-screenshot` or `spectacle`. Rune says what to install when none is
  found.

### Where the shot goes

The shot goes to the pane that had the keyboard when the picker opened, even
if the keyboard moves on. Screenshots require a directly connected host with
live organization and execution identity; older servers and hub satellite
panes refuse before opening a picker.

A disconnect, changed execution or session move cancels delivery, and so does
moving the window to another machine or retrying its connection. Private
uploads and their pasted paths are never replayed on reconnect. A lost
connection may leave the path's delivery uncertain: look before retrying.

### Limits and cleanup

Captures are bounded to 64 MiB, 8192 pixels per side and 256 MiB decoded. The
private local folder is removed, and closing the main window or quitting
cancels and reaps its picker. One capture runs at a time.

On Linux, exit 1 with no output or diagnostics is treated as dismissal, but
can also mean a silent tool failure; cancellation behavior still needs
qualification per picker and desktop. The uploaded file follows ordinary
server upload retention.
