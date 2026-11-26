import UIKit

/// A long press's menu as the system's own: Rune names what was held and
/// its verbs, UIKit opens the menu from the row Rune drew (Liquid Glass,
/// its own dimming and dismissal), and a pick runs back through Rune.
@MainActor
final class HeldMenu {
    /// A picked verb's id, for Rune to run.
    var picked: ((UInt64, String) -> Void)?
    var dismissed: ((UInt64) -> Void)?

    /// The menu's anchor: it opens the menu and never takes a touch, so the
    /// row under it stays Rune's.
    private final class Anchor: UIButton {
        var ended: (() -> Void)?
        override func point(inside point: CGPoint, with event: UIEvent?) -> Bool { false }
        override func contextMenuInteraction(
            _ interaction: UIContextMenuInteraction,
            willEndFor configuration: UIContextMenuConfiguration,
            animator: (any UIContextMenuInteractionAnimating)?
        ) {
            super.contextMenuInteraction(interaction, willEndFor: configuration, animator: animator)
            let ended = ended
            if let animator { animator.addCompletion { ended?() } }
            else { ended?() }
        }
    }
    private var current: (request: HeldMenuRequest, anchor: Anchor)?

    /// Revoke authority before a programmatic dismissal can reenter callbacks.
    func reconcile(_ request: UInt64?) {
        guard let old = current, old.request.id != request else { return }
        old.request.invalidate()
        current = nil
        old.anchor.contextMenuInteraction?.dismissMenu()
        old.anchor.removeFromSuperview()
    }

    /// Open `spec` (the frame's `menu` request) from its row in `host`.
    func show(_ spec: [String: Any], over host: UIView) {
        guard let id = spec["request"] as? UInt64,
              let rect = spec["rect"] as? [Double], rect.count == 4,
              let items = spec["items"] as? [[String: Any]] else { return }
        reconcile(id)
        let request = HeldMenuRequest(id)
        let anchor = Anchor(type: .custom)
        let actions = items.compactMap { item -> UIAction? in
            guard let id = item["id"] as? String, let title = item["title"] as? String else { return nil }
            let image = (item["symbol"] as? String).flatMap { UIImage(systemName: $0) }
            let destructive = item["destructive"] as? Bool == true
            return UIAction(title: title, image: image, attributes: destructive ? .destructive : []) { [weak self] _ in
                guard request.select() else { return }
                self?.picked?(request.id, id)
            }
        }
        guard !actions.isEmpty else { return }
        anchor.ended = { [weak self, weak anchor] in
            if request.disappear() { self?.dismissed?(request.id) }
            anchor?.removeFromSuperview()
            // Retain the request until explicit invalidation, not visual end.
            // Its already-selected action may arrive after this completion.
        }
        let menu = UIMenu(title: spec["title"] as? String ?? "", children: actions)
        if let subtitle = spec["subtitle"] as? String, !subtitle.isEmpty { menu.subtitle = subtitle }
        anchor.menu = menu
        anchor.showsMenuAsPrimaryAction = true
        anchor.isAccessibilityElement = false
        anchor.frame = CGRect(x: rect[0], y: rect[1], width: max(rect[2], 1), height: max(rect[3], 1))
        current = (request, anchor)
        if anchor.superview !== host { host.insertSubview(anchor, at: 0) }
        anchor.performPrimaryAction()
    }
}
