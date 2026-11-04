**Phux Rune 0.1.0-alpha.10**: every machine in the sidebar, and idle that tells you what's still running.

- **Machines in the sidebar.** Your sessions sit under this machine's name,
  which folds them away. Every other machine follows, folded; open one to see
  its sessions, and press one to move the window there and open it.
- **Background work.** A terminal that has gone quiet but still has something
  running behind it, such as a job sent off with `&` or an agent's background
  task, shows a hollow ring and the word "background" instead of a plain idle
  dot. Needs a phux server from 0.118.0 on.
- **Copy and paste on Omarchy.** Super+C and Super+V copy and paste, as in
  Omarchy's own terminals, instead of interrupting what's running. Rune now
  names its window `sh.phux.rune`; `phux omarchy setup` from the next phux release tags it a
  terminal. Ctrl+Insert and Shift+Insert copy and paste everywhere on Linux.
- **More keys reach the terminal.** Insert, Print Screen, Scroll Lock and
  Pause now go through to what runs there.
