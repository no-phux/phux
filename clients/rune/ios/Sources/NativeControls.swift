import UIKit

/// The phone's floating controls as UIKit Liquid Glass. Rune lays each out,
/// names it and runs its press; it leaves an empty box where it sits and
/// lists it in each drawn frame's `controls`. Glass over a Metal layer must
/// own its content, or it would blur Rune's label beneath it, so the symbol
/// and title are the button's own. A control the frame no longer lists (a
/// page or sheet covers it, or glass may not draw) is removed.
@MainActor
final class NativeControls {
    private struct Look: Equatable {
        var label: String
        var title: String?
        var subtitle: String?
        var subtitleInk: [Double]?
        var leading: Bool
        var symbol: String
        var enabled: Bool
        var dark: Bool
        var ink: [Double]
        var badge: [Double]?
        var circle: Bool
    }

    @MainActor
    private final class Control {
        let button: UIButton
        let badge = UIView()
        var look: Look?
        init(button: UIButton) { self.button = button }
    }

    /// Weak, not unowned: a closing surface removes its controls from its
    /// own deinit, when every reference to it is already dead.
    private weak var host: UIView?
    private var controls: [String: Control] = [:]
    /// A tap on the control `id`; true when Rune ran its press.
    var pressed: ((String) -> Void)?

    init(host: UIView) { self.host = host }

    /// Bring the buttons to the frame's list: where Rune drew each, in its
    /// reading order. Moves follow Rune's own motion, so UIKit animates none.
    func update(_ specs: [[String: Any]]) {
        var seen = Set<String>()
        var ordered: [UIView] = []
        UIView.performWithoutAnimation {
            for spec in specs {
                guard let id = spec["id"] as? String, let rect = spec["rect"] as? [Double],
                      rect.count == 4, let look = Self.look(spec, rect: rect) else { continue }
                seen.insert(id)
                let control = controls[id] ?? make(id)
                if control.look != look { apply(look, to: control) }
                let frame = CGRect(x: rect[0], y: rect[1], width: rect[2], height: rect[3])
                if control.button.frame != frame {
                    control.button.frame = frame
                    control.badge.frame.origin.x = frame.width - 15
                }
                let alpha = CGFloat(spec["opacity"] as? Double ?? 1)
                if control.button.alpha != alpha { control.button.alpha = alpha }
                // Repeated ids keep their final occurrence's reading order.
                ordered.removeAll { $0 === control.button }
                ordered.append(control.button)
            }
            for (id, control) in controls where !seen.contains(id) {
                control.button.removeFromSuperview()
                controls.removeValue(forKey: id)
            }
            order(ordered)
        }
    }

    private func order(_ ordered: [UIView]) {
        // Reordering unchanged controls marks the host for layout, which
        // draws again. Check the real hierarchy so new non-controls still
        // stay beneath the buttons, without perpetuating idle frames.
        guard let host, !host.subviews.suffix(ordered.count).elementsEqual(ordered, by: { $0 === $1 }) else { return }
        for button in ordered { host.bringSubviewToFront(button) }
    }

    func removeAll() { update([]) }

    /// Simulator qualification: the glass controls UIKit shows now.
    var evidence: [[String: Any]] {
        controls.sorted { $0.key < $1.key }.map { id, control in
            let frame = control.button.frame
            return ["id": id, "frame": [frame.minX, frame.minY, frame.width, frame.height],
                    "enabled": control.button.isEnabled, "label": control.button.accessibilityLabel ?? "",
                    "shown": control.button.window != nil && control.button.alpha > 0.01]
        }
    }

    /// Simulator qualification: tap `id`'s real button, as a finger would.
    func tap(_ id: String) -> Bool {
        guard let button = controls[id]?.button, button.isEnabled, button.window != nil else { return false }
        button.sendActions(for: .primaryActionTriggered)
        return true
    }

    private static func look(_ spec: [String: Any], rect: [Double]) -> Look? {
        guard let label = spec["label"] as? String, let symbol = spec["symbol"] as? String,
              let ink = spec["ink"] as? [Double], ink.count == 3 else { return nil }
        let badge = (spec["badge"] as? [Double]).flatMap { $0.count == 3 ? $0 : nil }
        let subtitleInk = (spec["subtitle_ink"] as? [Double]).flatMap { $0.count == 3 ? $0 : nil }
        return Look(label: label, title: spec["title"] as? String, subtitle: spec["subtitle"] as? String,
                    subtitleInk: subtitleInk, leading: spec["leading"] as? Bool ?? false, symbol: symbol,
                    enabled: spec["enabled"] as? Bool ?? true, dark: spec["dark"] as? Bool ?? true,
                    ink: ink, badge: badge, circle: abs(rect[2] - rect[3]) < 1)
    }

    private func make(_ id: String) -> Control {
        let button = UIButton(type: .system)
        let control = Control(button: button)
        button.addAction(UIAction { [weak self] _ in self?.pressed?(id) }, for: .primaryActionTriggered)
        control.badge.isUserInteractionEnabled = false
        control.badge.isAccessibilityElement = false
        button.addSubview(control.badge)
        host?.addSubview(button)
        controls[id] = control
        return control
    }

    private func apply(_ look: Look, to control: Control) {
        let button = control.button
        let ink = Self.color(look.ink)
        var configuration = UIButton.Configuration.glass()
        configuration.cornerStyle = .capsule
        configuration.baseForegroundColor = ink
        let weight: UIImage.SymbolWeight = .semibold
        configuration.preferredSymbolConfigurationForImage =
            UIImage.SymbolConfiguration(pointSize: look.circle ? 17 : 15, weight: weight)
        configuration.image = UIImage(systemName: look.symbol)
        if look.leading {
            // A navigation title: where you are, what it is doing, and a
            // chevron saying it opens a choice.
            let title = UIFontMetrics(forTextStyle: .body)
                .scaledFont(for: .systemFont(ofSize: 17, weight: .semibold), maximumPointSize: 20)
            let detail = UIFontMetrics(forTextStyle: .footnote)
                .scaledFont(for: .systemFont(ofSize: 13, weight: .regular), maximumPointSize: 16)
            configuration.attributedTitle = AttributedString(look.title ?? "", attributes: AttributeContainer([.font: title]))
            if let subtitle = look.subtitle, !subtitle.isEmpty {
                var attributes = AttributeContainer([.font: detail])
                attributes.foregroundColor = look.subtitleInk.map(Self.color) ?? ink.withAlphaComponent(0.6)
                configuration.attributedSubtitle = AttributedString(subtitle, attributes: attributes)
            }
            configuration.titleAlignment = .leading
            configuration.titlePadding = 1
            configuration.titleLineBreakMode = .byTruncatingTail
            configuration.subtitleLineBreakMode = .byTruncatingTail
            configuration.imagePlacement = .trailing
            configuration.imagePadding = 6
            // A quiet chevron: the glass's foreground would draw it at full ink.
            let chevron = UIImage.SymbolConfiguration(pointSize: 11, weight: .semibold)
            configuration.image = UIImage(systemName: look.symbol, withConfiguration: chevron)?
                .withTintColor(ink.withAlphaComponent(0.45), renderingMode: .alwaysOriginal)
            configuration.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 18, bottom: 0, trailing: 18)
            button.contentHorizontalAlignment = .leading
        } else if let title = look.title {
            // Rune laid out a touch-height capsule; the title grows with
            // Dynamic Type until it would crowd that height, then truncates.
            let font = UIFontMetrics(forTextStyle: .body)
                .scaledFont(for: .systemFont(ofSize: 17, weight: .semibold), maximumPointSize: 24)
            configuration.attributedTitle = AttributedString(title, attributes: AttributeContainer([.font: font]))
            configuration.titleLineBreakMode = .byTruncatingTail
            configuration.imagePadding = 8
            configuration.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 16, bottom: 0, trailing: 16)
        } else {
            configuration.contentInsets = .zero
        }
        button.configuration = configuration
        button.overrideUserInterfaceStyle = look.dark ? .dark : .light
        button.isEnabled = look.enabled
        button.accessibilityLabel = look.label
        button.largeContentTitle = look.title ?? look.label
        button.showsLargeContentViewer = true
        if let badge = look.badge {
            // Rune's own place for it: the upper trailing shoulder.
            control.badge.backgroundColor = Self.color(badge)
            control.badge.frame = CGRect(x: button.bounds.width - 15, y: 7, width: 8, height: 8)
            control.badge.layer.cornerRadius = 4
            control.badge.isHidden = false
        } else {
            control.badge.isHidden = true
        }
        control.look = look
    }

    private static func color(_ srgb: [Double]) -> UIColor {
        UIColor(red: srgb[0], green: srgb[1], blue: srgb[2], alpha: 1)
    }
}
