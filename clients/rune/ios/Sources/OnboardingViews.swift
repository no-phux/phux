import UIKit

/// The one thing a pairing stage asks or says, centred above its buttons so
/// it cannot be mistaken for another list row. A header-footer view, not a
/// bare view: an inset table measures only those at the width it gives them.
@MainActor
final class OnboardingHero: UITableViewHeaderFooterView {
    let title = UILabel()
    let detail = UILabel()

    /// A nil `symbol` shows progress in its place.
    init(symbol: String?, title heading: String, detail body: String) {
        super.init(reuseIdentifier: nil)
        let mark: UIView
        if let symbol {
            let style = UIImage.SymbolConfiguration(textStyle: .largeTitle, scale: .large)
            let image = UIImageView(image: UIImage(systemName: symbol, withConfiguration: style))
            image.contentMode = .scaleAspectFit
            image.adjustsImageSizeForAccessibilityContentSizeCategory = true
            mark = image
        } else {
            let spinner = UIActivityIndicatorView(style: .large)
            spinner.startAnimating()
            spinner.translatesAutoresizingMaskIntoConstraints = false
            mark = UIView()
            mark.addSubview(spinner)
            NSLayoutConstraint.activate([
                spinner.centerXAnchor.constraint(equalTo: mark.centerXAnchor),
                spinner.topAnchor.constraint(equalTo: mark.topAnchor),
                spinner.bottomAnchor.constraint(equalTo: mark.bottomAnchor)])
        }
        title.text = heading
        title.font = UIFontMetrics(forTextStyle: .title2).scaledFont(for: .systemFont(ofSize: 22, weight: .bold))
        title.accessibilityTraits = .header
        detail.text = body
        detail.font = .preferredFont(forTextStyle: .body)
        detail.textColor = .secondaryLabel
        detail.isHidden = body.isEmpty
        for label in [title, detail] {
            label.numberOfLines = 0
            label.textAlignment = .center
            label.adjustsFontForContentSizeCategory = true
            // The name and host are what the person checks; never trade them for height.
            label.setContentCompressionResistancePriority(.required, for: .vertical)
        }
        let stack = UIStackView(arrangedSubviews: [mark, title, detail])
        stack.axis = .vertical
        stack.alignment = .fill
        stack.spacing = 10
        stack.setCustomSpacing(18, after: mark)
        stack.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(stack)
        // A header is measured at zero height first; yield to that pass and nothing else.
        let bottom = stack.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -24)
        bottom.priority = .required - 1
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 32), bottom,
            stack.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -20)])
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
}

/// A real button in the list: filled when it is the stage's one decision,
/// so confirming a pairing never looks like opening another row.
@MainActor
final class PairingActionCell: UITableViewCell {
    let button: UIButton

    init(title: String, prominent: Bool, tint: UIColor, action: @escaping () -> Void) {
        var style: UIButton.Configuration = prominent ? .borderedProminent() : .plain()
        style.title = title
        style.buttonSize = .large
        style.baseBackgroundColor = prominent ? tint : nil
        style.baseForegroundColor = prominent ? .white : tint
        if prominent {
            style.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { incoming in
                var outgoing = incoming
                outgoing.font = UIFont.preferredFont(forTextStyle: .headline)
                return outgoing
            }
        }
        button = UIButton(configuration: style)
        super.init(style: .default, reuseIdentifier: nil)
        selectionStyle = .none
        backgroundConfiguration = .clear()
        button.addAction(UIAction { _ in action() }, for: .primaryActionTriggered)
        button.titleLabel?.adjustsFontForContentSizeCategory = true
        button.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(button)
        NSLayoutConstraint.activate([
            button.topAnchor.constraint(equalTo: contentView.topAnchor, constant: prominent ? 0 : 4),
            button.bottomAnchor.constraint(equalTo: contentView.bottomAnchor),
            button.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            button.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            button.heightAnchor.constraint(greaterThanOrEqualToConstant: prominent ? 52 : 44)])
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
}

extension UITableViewCell {
    /// A titled row. Its text wraps and its height follows, with or without
    /// an accessory view, which the legacy cell labels get wrong.
    @MainActor
    static func titled(_ title: String, detail: String?, icon: String?, tint: UIColor,
                       dimmed: Bool = false) -> UITableViewCell {
        let cell = UITableViewCell(style: .default, reuseIdentifier: nil)
        var content = UIListContentConfiguration.subtitleCell()
        content.text = title
        content.secondaryText = detail
        content.textProperties.color = dimmed ? .secondaryLabel : .label
        content.secondaryTextProperties.color = dimmed ? .tertiaryLabel : .secondaryLabel
        content.secondaryTextProperties.font = .preferredFont(forTextStyle: .subheadline)
        content.secondaryTextProperties.adjustsFontForContentSizeCategory = true
        content.textToSecondaryTextVerticalPadding = 3
        content.image = icon.flatMap { UIImage(systemName: $0) }
        content.imageProperties.tintColor = dimmed ? .tertiaryLabel : tint
        cell.contentConfiguration = content
        if dimmed { cell.accessibilityTraits.insert(.notEnabled) }
        return cell
    }
}

extension UIAlertController {
    /// An alert takes the window's appearance, not its presenter's. Setup is
    /// one dark surface, so its alerts are dark on a light-mode phone too.
    @MainActor
    static func dark(title: String, message: String) -> UIAlertController {
        let alert = UIAlertController(title: title, message: message, preferredStyle: .alert)
        alert.overrideUserInterfaceStyle = .dark
        return alert
    }
}
