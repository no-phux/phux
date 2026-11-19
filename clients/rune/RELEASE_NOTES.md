**Phux Rune 0.1.43**: more readable, reachable Settings controls with large text.

- Keep all five Settings size-adjustment rows readable and their minus/value/plus groups reachable at the largest accessibility text size. Phone rows wrap the label above the controls and enlarge the value readout, preserving the requested typography. Desktop composition and existing adjustment actions are unchanged; this does not change the existing Interface zoom behavior. (#2683)
- Advance Rune from `78efd4a` to `1ff57939`, including the Wayland content/frame corner fix and its new `sctk-adwaita` patch. Adopt the empty-field accessibility correction, covering empty → Unicode → clear → retype transitions in the actual Settings runtime. (#2683)
- Retain payload-free native connection, capture-health and server-publication diagnostics for isolated qualification. A recovered qualification timeout is not a root-cause fix. (#2677)
