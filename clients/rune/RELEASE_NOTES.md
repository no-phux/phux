**Phux Rune 0.1.4**: current with Rune dev `3ff3cc6`, plus Pitch.

- Open **Pitch** from the palette to see agents as a football squad:
  working players drill and pass, waiting players raise a hand, and finished
  players celebrate. Select a player, then press again or Return to open
  its terminal.
- Upstream's desktop host now bounds overdue timer frames when a covered
  Wayland surface never receives its redraw callback, avoiding a busy loop.
- Motion started by a slow desktop/overlay frame no longer jumps ahead on
  the next frame. Existing running motion retains real elapsed time.
- Rune's consecutive fold rows now open and close as one block.

The native UIKit host still passes paired-connection, input and lifecycle
checks. The new private late-frame presentation hook is not yet called by
upstream's embedded host, so this release does not claim that timing fix
for native iOS. Device/TestFlight distribution remains separate work.
