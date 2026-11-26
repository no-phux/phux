# Phux Rune on a phone: stand-ins and next steps

What the [phone build](./MOBILE.md) still stands in for, and what replaces it.

## Stand-ins and what replaces them

| Stand-in | Waits for, upstream | Then |
| --- | --- | --- |
| The drawn keyboard in desktop `--phone` | Native UIKit host already replaces it on iOS | Keep it for desktop phone qualification; qualify real OS keyboard/CJK/dictation on devices before claiming them |
| Liquid Glass reaches only the floating symbol and dock controls, as UIKit buttons over the Metal layer. The accessory key bar, drag pad, pane map, tab pills, page headers and sheets stay Rune-drawn (the Sessions drawer is Rune's frosted glass, the rest opaque) (their touch ownership or content is Rune's); a UIKit control follows Rune's drawn frame, so it can trail a moving Rune surface by up to a frame during a transition. The desktop-hosted phone draws the dock opaque over the drawer's frosted glass, the phone's one blur | Rune's native UIKit material support (`SystemMaterial::Glass` is macOS-only today) | Glass under Rune-owned surfaces without a UIKit view per control |
| Fixed status/home insets and keyboard height in desktop `--phone` | Already replaced by UIKit layout guides in the native iOS host | Keep desktop snapshots deterministic; qualify rotation, floating keyboards and physical-device safe areas |
| Ad-hoc device updates do not qualify physical-device input/GPU behavior or TestFlight distribution; Android remains a desktop preview | Physical-device qualification and App Store distribution; an Android activity/lifecycle host | Qualify the installed signed build before a TestFlight lane; Android is a separate host project |
| First-finger presses and drags; extra fingers are ignored. The terminal owns its selection/program hold policy and times it from the app clock, since a drag carries no time. Pane navigation keeps a dedicated handle so it never steals a terminal drag; sideways drags page sessions and the left edge pulls the Sessions drawer | Multi-touch gesture arbitration | Two fingers for panes (the native host already pinches for text size through UIKit) |
| Haptics only where the native host plays them: a hold, a copy and a swipe's commit detent | A haptics API | A tick on latch, tab switch and pad step |
| No native Rune VoiceOver element bridge or image copy to the clipboard (text copy, paste, attachments and links go through the native host) | App/platform adapters over upstream's runtime | Qualify the native accessibility tree and service these through UIKit; desktop accessibility is not iOS evidence |

Upstream `99121250` supplies the touch-ownership seam that was missing in
`740ecbc9`: terminal and dedicated drag surfaces opt out of automatic long
press through `touch_drags_at_once`. Runtime-clock regressions cover stationary,
dragged and Shift-local holds, program-button releases, held navigation and
pad cancellation. The terminal's hold remains selection, not a context menu.

## Next, no upstream needed

| Piece | Today |
| --- | --- |
| Phone machine sheet | The desktop's, narrowed to touch-sized rows and controls; a phone-native grouped presentation is still to come |
