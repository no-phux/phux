import UIKit

/// A long press's menu as the system's own: Rune names what was held and
/// its verbs, UIKit opens the menu from the row Rune drew (Liquid Glass,
/// its own dimming and dismissal), and a pick runs back through Rune.
@MainActor
final class HeldMenu {
    /// A picked verb's id, for Rune to run.
    var picked: ((String) -> Void)?

    /// The menu's anchor: it opens the menu and never takes a touch, so the
    /// row under it stays Rune's.
    private final class Anchor: UIButton {
        override func point(inside point: CGPoint, with event: UIEvent?) -> Bool { false }
    }
    private let anchor = Anchor(type: .custom)

    /// Open `spec` (the frame's `menu` request) from its row in `host`.
    func show(_ spec: [String: Any], over host: UIView) {
        guard let rect = spec["rect"] as? [Double], rect.count == 4,
              let items = spec["items"] as? [[String: Any]] else { return }
        let actions = items.compactMap { item -> UIAction? in
            guard let id = item["id"] as? String, let title = item["title"] as? String else { return nil }
            let image = (item["symbol"] as? String).flatMap { UIImage(systemName: $0) }
            let destructive = item["destructive"] as? Bool == true
            return UIAction(title: title, image: image, attributes: destructive ? .destructive : []) { [weak self] _ in
                self?.picked?(id)
            }
        }
        guard !actions.isEmpty else { return }
        let menu = UIMenu(title: spec["title"] as? String ?? "", children: actions)
        if let subtitle = spec["subtitle"] as? String, !subtitle.isEmpty { menu.subtitle = subtitle }
        anchor.menu = menu
        anchor.showsMenuAsPrimaryAction = true
        anchor.isAccessibilityElement = false
        anchor.frame = CGRect(x: rect[0], y: rect[1], width: max(rect[2], 1), height: max(rect[3], 1))
        if anchor.superview !== host { host.insertSubview(anchor, at: 0) }
        anchor.performPrimaryAction()
    }
}
