"""Execute the shipped menu callbacks with inert UIKit adapters, not a simulator."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ios"))
import build


ADAPTERS = """
import Foundation
import CoreGraphics
@MainActor class UIEvent {}
@MainActor class UIContextMenuConfiguration {}
@MainActor protocol UIContextMenuInteractionAnimating {
    func addCompletion(_ completion: @escaping () -> Void)
}
@MainActor final class Animator: UIContextMenuInteractionAnimating {
    var completions: [() -> Void] = []
    func addCompletion(_ completion: @escaping () -> Void) { completions.append(completion) }
    func finish() { for completion in completions { completion() } }
}
@MainActor class UIContextMenuInteraction {
    var onDismiss: (() -> Void)?
    func dismissMenu() { onDismiss?() }
}
@MainActor class UIView {
    var superview: UIView?
    var children: [UIView] = []
    func insertSubview(_ view: UIView, at index: Int) {
        children.insert(view, at: index)
        view.superview = self
    }
    func removeFromSuperview() {
        superview?.children.removeAll { $0 === self }
        superview = nil
    }
}
@MainActor class UIButton: UIView {
    enum Kind { case custom }
    init(type: Kind) {}
    var menu: UIMenu?
    var showsMenuAsPrimaryAction = false
    var isAccessibilityElement = true
    var frame = CGRect.zero
    var contextMenuInteraction: UIContextMenuInteraction? = UIContextMenuInteraction()
    func point(inside point: CGPoint, with event: UIEvent?) -> Bool { true }
    func performPrimaryAction() {}
    func contextMenuInteraction(_ interaction: UIContextMenuInteraction,
        willEndFor configuration: UIContextMenuConfiguration,
        animator: (any UIContextMenuInteractionAnimating)?) {}
}
@MainActor class UIImage { init?(systemName: String) {} }
@MainActor class UIAction {
    struct Attributes: OptionSet {
        let rawValue: Int
        static let destructive = Attributes(rawValue: 1)
    }
    let handler: (UIAction) -> Void
    init(title: String, image: UIImage?, attributes: Attributes,
         handler: @escaping (UIAction) -> Void) { self.handler = handler }
    func invoke() { handler(self) }
}
@MainActor class UIMenu {
    let children: [UIAction]
    var subtitle: String?
    init(title: String, children: [UIAction]) { self.children = children }
}
"""

CHECKS = """
@MainActor func show(_ menu: HeldMenu, _ host: UIView, _ request: UInt64) -> UIButton {
    menu.show(["request": request, "rect": [1.0, 2.0, 44.0, 44.0],
               "items": [["id": "fold", "title": "Fold"]]], over: host)
    return host.children.first as! UIButton
}
@MainActor func end(_ button: UIButton, _ animator: Animator?) {
    button.contextMenuInteraction(button.contextMenuInteraction!,
        willEndFor: UIContextMenuConfiguration(), animator: animator)
}
// Actual callbacks must accept a selected action before, during, or after
// visual disappearance. UIKit promises no ordering between these callbacks.
for order in 0..<3 {
    let menu = HeldMenu(), host = UIView(), animator = Animator()
    var picked: [UInt64] = [], dismissed: [UInt64] = []
    menu.picked = { request, verb in
        precondition(verb == "fold")
        picked.append(request)
    }
    menu.dismissed = { dismissed.append($0) }
    let button = show(menu, host, 41), action = button.menu!.children[0]
    if order == 0 { action.invoke() }
    end(button, animator)
    if order == 1 { action.invoke() }
    animator.finish()
    if order == 2 { action.invoke() }
    action.invoke()
    animator.finish()
    precondition(picked == [41], "one action, its original request")
    precondition(dismissed == (order == 2 ? [41] : []))
    precondition(host.children.isEmpty, "only the ended anchor is removed")
}
// A delayed end/action from A cannot clear B or borrow its identity.
let menu = HeldMenu(), host = UIView(), animator = Animator()
var picked: [UInt64] = [], dismissed: [UInt64] = []
menu.picked = { request, _ in picked.append(request) }
menu.dismissed = { dismissed.append($0) }
let old = show(menu, host, 51), oldAction = old.menu!.children[0]
end(old, animator)
let new = show(menu, host, 52), newAction = new.menu!.children[0]
animator.finish()
oldAction.invoke()
precondition(picked.isEmpty && dismissed.isEmpty)
precondition(host.children.count == 1 && host.children[0] === new)
newAction.invoke()
precondition(picked == [52])
// Invalidation precedes synchronous reentry during programmatic dismissal.
let revoked = show(menu, host, 53), revokedAction = revoked.menu!.children[0]
revoked.contextMenuInteraction!.onDismiss = { revokedAction.invoke() }
menu.reconcile(nil)
precondition(picked == [52])
precondition(host.children.isEmpty)
// Nil animator and repeated callbacks still report visual dismissal only once.
let last = show(menu, host, 54), lastAction = last.menu!.children[0]
end(last, nil)
end(last, nil)
precondition(dismissed == [54])
lastAction.invoke()
precondition(picked == [52, 54], "visual dismissal alone is not cancellation")
"""


@unittest.skipUnless(sys.platform == "darwin", "Swift host execution needs Xcode")
class HeldMenuTests(unittest.TestCase):
    def test_shipped_callbacks_preserve_request_identity_and_lifecycle(self):
        sources = Path(__file__).resolve().parents[1] / "ios/Sources"
        request = (sources / "HeldMenuRequest.swift").read_text()
        menu = (sources / "HeldMenu.swift").read_text().replace("import UIKit", "")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            main = root / "main.swift"
            main.write_text(ADAPTERS + request + menu + CHECKS)
            compiled = subprocess.run(
                ["/usr/bin/xcrun", "--sdk", "macosx", "swiftc", "-swift-version", "6",
                 "-warnings-as-errors", str(main), "-o", str(root / "menu")],
                env=build.apple_environment(root), capture_output=True, text=True, timeout=120)
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            ran = subprocess.run([str(root / "menu")], capture_output=True, text=True, timeout=10)
            self.assertEqual(ran.returncode, 0, ran.stderr)


if __name__ == "__main__":
    unittest.main()
