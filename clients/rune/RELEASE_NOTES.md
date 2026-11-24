**Phux Rune 0.1.51**

- Keep question choices and unfinished replies with the right agent, even when
  different agents reuse the same question identifier.
- Retire replaced editors safely: queued composition, Return and late phone
  touches cannot answer a replacement question or reach its terminal.
- Group phone form labels, help and controls without shrinking the 44-point
  touch rows.
- Use the shared palette matcher and show shortcuts as key caps.
- Avoid redundant native control reorders and unchanged accessibility updates.
- Update the Rune engine to a compatible upstream dev snapshot, including
  unchanged-frame IME synchronization.

Optional provider form elicitation is still unsupported. The newer strict-modal
upstream change remains deferred; this release does not bypass input ownership.
