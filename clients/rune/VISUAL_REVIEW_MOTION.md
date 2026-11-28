# Visual review: motion

The motion cases of [visual review](./VISUAL_REVIEW.md): each exports
fixed-clock frames whose `.motion.json` sidecars bind the case, stage,
frame and elapsed time into the manifest, with the values the frame was
drawn with. They establish rendered trajectories and input semantics,
not physical-device smoothness.

Phone pane-motion cases exercise the actual Move handler over a retained,
unequal split arrangement. They export idle, press, first-axis, mid-drag,
release, release +100 ms and settled frames at a fixed 60 Hz. Matching
`.motion.json` sidecars bind each case, frame and elapsed time into the
manifest. Horizontal/vertical drift, reversal, boundary resistance,
cancellation, reduced motion and keyboard-open cases supplement the small,
light/dark and native-sized 2× matrix. They establish rendered trajectories
and input semantics, not physical-device smoothness.

Batched swipe cases queue two moves and a release before one frame. Compare
idle, start, release, release +100 ms and settled edge-pull frames on both
phone widths, full and native keyboard-reduced viewports, light/dark and
reduced motion at true 2×. Their motion sidecars bind the actual fixed-frame
timeline. The final displacement must determine whether Sessions opens;
behavior tests also cover tabs, session boundaries, withdrawal, cancellation
and untimed velocity accumulation. These cases do not establish physical
touch feel or a velocity estimate for moves with no elapsed app time.

Dismiss cases swipe each page away through `mobile::touch` on both widths,
light/dark and reduced motion; they assert one-to-one drawing and no commit
per move, and sidecars record the drawn values. A sheet resting on the bottom
edge (rename) is held by its header.

Page motion cases (`page_motion_visual_review_frames`) press each page's own
control: idle, the commit, 50, 100 and 200 ms and settled, on both widths,
light and dark, reduced motion, and AX5 with the drawn keyboard up. They
assert each page travels its own way (a sheet up whole and opaque, Sessions
from the left, a Settings level from the trailing edge), lands at once under
reduced motion, and never flattens glass inside a fading layer; sidecars
record every page's drawn rectangle and opacity. Switcher swipe cases
(`switcher_swipe_visual_review_frames`) page the preview card through Rune's
touch path and assert one-to-one drawing mid-drag and a settled next card.
