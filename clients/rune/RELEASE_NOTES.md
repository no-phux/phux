**Phux Rune 0.1.52**

- Wrap phone reconnect messages and recovery actions instead of truncating
  long names or clipping buttons.
- Let the recovery card scroll on short screens and with larger text, while
  keeping terminal input shielded and keyboard focus rings visible.
- Repair automatic patch releases when no bump pull request is open.

The Rune engine pin is unchanged. The newer strict-modal upstream change remains
deferred; this release preserves terminal input ownership and pane arrangement.
