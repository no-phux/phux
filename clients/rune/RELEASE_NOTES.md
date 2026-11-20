**Phux Rune 0.1.44**: changes since public 0.1.42. No public 0.1.43 release was tagged.

- Keep phone Settings **Text size** and **Interface size** controls readable and their minus/value/plus groups reachable at the largest accessibility text size. Labels wrap above the controls and values are larger, without shrinking the requested text. Desktop layout and existing Interface zoom behavior are unchanged. (#2683)
- Update Rune from `78efd4a` to `1ff57939`. Adopt Rune's empty-field accessibility-tree correction, including empty → Unicode → clear → retype transitions, and the Wayland content/frame corner fix with its `sctk-adwaita` patch. (#2683)
- Strengthen isolated native simulator qualification with payload-free enrollment/publication evidence, sealed preparation-failure captures, and Apple's boot-readiness check. These are development diagnostics and gates, not new phone logging or a claimed performance or enrollment-latency fix. (#2677, #2690)
