import UIKit

/// First run is Scan, Paste or Find a computer—not a credential form. A
/// staged pairing takes the whole screen, so confirming is never one row
/// among other ways to connect.
@MainActor
final class OnboardingController: UITableViewController {
    private enum Screen { case choices, review, enrolling, finish }

    var connected: (([String: Any]) -> Void)?
    var closed: (() -> Void)?
    private let saved: [String: Any]?
    /// The computer this phone already opens; pairing another replaces it.
    private let current: String?
    let flow: PairingFlow
    private var camera: Task<Void, Never>?
    private var displayed = Screen.choices
    /// A problem no alert could deliver, kept in the list until one can.
    private var notice: PairingFlow.Problem?

    init(values: [String: Any]?, flow: PairingFlow = PairingFlow()) {
        saved = values
        self.flow = flow
        current = values.map { _ in HostSettings.computerName ?? "your computer" }
        super.init(style: .insetGrouped)
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
    isolated deinit { camera?.cancel() }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = current == nil ? "Connect a computer" : "Change computer"
        overrideUserInterfaceStyle = .dark
        navigationItem.leftBarButtonItem = UIBarButtonItem(systemItem: .cancel,
            primaryAction: UIAction { [weak self] _ in self?.cancel() })
        pasteConfiguration = UIPasteConfiguration(forAccepting: NSString.self)
        flow.changed = { [weak self] in self?.refresh() }
        flow.failed = { [weak self] in self?.interrupt($0) }
        flow.connected = { [weak self] values in
            UINotificationFeedbackGenerator().notificationOccurred(.success)
            self?.connected?(values)
        }
        tableView.accessibilityIdentifier = "rune-onboarding"
        tableView.sectionHeaderHeight = UITableView.automaticDimension
        tableView.estimatedSectionHeaderHeight = 160
        registerForTraitChanges([UITraitPreferredContentSizeCategory.self]) { (self: Self, _: UITraitCollection) in
            self.tableView.reloadData() // Header heights are not re-measured on their own.
        }
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        // The bar belongs to the sheet, not to this screen; keep both dark.
        navigationController?.overrideUserInterfaceStyle = .dark
    }

    @discardableResult
    func receive(_ link: String) -> Bool {
        loadViewIfNeeded()
        let accepted = flow.receive(link)
        if accepted { tableView.scrollToRow(at: IndexPath(row: 0, section: 0), at: .top, animated: false) }
        return accepted
    }

    private var screen: Screen {
        switch flow.stage {
        case .welcome: .choices
        case .review: .review
        case .enrolling: .enrolling
        case .finish: .finish
        }
    }

    private func refresh() {
        let previous = displayed
        displayed = screen
        if flow.problem == nil { notice = nil }
        isModalInPresentation = displayed == .enrolling
        tableView.separatorStyle = displayed == .choices ? .singleLine : .none
        guard displayed != previous, view.window != nil else { tableView.reloadData(); return }
        UIView.transition(with: tableView, duration: 0.2, options: .transitionCrossDissolve) {
            self.tableView.reloadData()
        }
        UIAccessibility.post(notification: .screenChanged, argument: nil)
    }

    private func cancel() {
        camera?.cancel(); camera = nil
        flow.cancel()
        dismiss(animated: true, completion: closed)
    }

    override func numberOfSections(in tableView: UITableView) -> Int { screen == .choices ? 3 : 1 }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        switch screen {
        case .choices: section == 0 ? (notice == nil ? 1 : 2) : section == 1 ? 3 : 1
        case .review, .finish: 2
        case .enrolling: 0
        }
    }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        guard screen == .choices else { return nil }
        return [current == nil ? "Your terminals, wherever you are" : "This phone's computer",
                "Choose how to connect", "More options"][section]
    }

    override func tableView(_ tableView: UITableView, viewForHeaderInSection section: Int) -> UIView? {
        switch flow.stage {
        case .welcome: nil
        case .review(let invite):
            OnboardingHero(symbol: "desktopcomputer", title: "Pair with \(invite.name)?",
                detail: "This phone will connect to \(invite.host). Only continue if that's your computer.")
        case .enrolling(let name):
            OnboardingHero(symbol: nil, title: "Pairing with \(name)…",
                detail: "Giving this phone its own key. This takes a few seconds.")
        case .finish:
            OnboardingHero(symbol: "key", title: "Almost there", detail: flow.message ?? "")
        }
    }

    override func tableView(_ tableView: UITableView, titleForFooterInSection section: Int) -> String? {
        switch (screen, section) {
        case (.choices, 0):
            current.map { "Pairing with another computer replaces \($0) on this phone." }
                ?? "Nothing to type. Pairing gives this phone its own key for your computer."
        case (.choices, 1):
            "On your computer, run \(pairCommand). Scan the QR code it shows, or copy its link and paste it here. Tailscale can find your computers and fetch the code for you."
        case (.choices, _): "Only for troubleshooting or a custom address."
        case (.review, _): flow.message
        default: nil
        }
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        switch screen {
        case .choices: choice(at: indexPath)
        case .review where indexPath.row == 0: action("Pair & Connect", "confirm-pairing") { $0.confirm() }
        case .finish where indexPath.row == 0: action("Try Again", "retry-pairing-save") { $0.confirm() }
        default: action("Cancel", "cancel-pairing", prominent: false) { $0.cancel() }
        }
    }

    private func action(_ title: String, _ identifier: String, prominent: Bool = true,
                        _ perform: @escaping (PairingFlow) -> Void) -> UITableViewCell {
        let cell = PairingActionCell(title: title, prominent: prominent, tint: view.tintColor) { [weak self] in
            if let self { perform(self.flow) }
        }
        cell.accessibilityIdentifier = identifier
        return cell
    }

    private func choice(at indexPath: IndexPath) -> UITableViewCell {
        switch (indexPath.section, indexPath.row) {
        case (0, 0): return status()
        case (0, _):
            let cell = row(notice?.title ?? "", detail: notice?.message ?? "",
                           icon: "exclamationmark.triangle.fill", tint: .systemOrange, tappable: false)
            cell.accessibilityIdentifier = "pairing-notice"
            return cell
        case (2, _):
            return row("Advanced setup", detail: "Enter connection settings by hand", icon: "slider.horizontal.3")
        default:
            let choices = [("Scan pairing QR", "Use this phone's camera", "qrcode.viewfinder"),
                           ("Paste pairing link", "Use the link you copied from your computer", "doc.on.clipboard"),
                           ("Find with Tailscale", "Sign in and choose a computer", "network")]
            let choice = choices[indexPath.row]
            let cell = row(choice.0, detail: choice.1, icon: choice.2)
            cell.accessibilityIdentifier = ["scan-pairing", "paste-pairing", "find-tailnet"][indexPath.row]
            if indexPath.row == 1 { cell.accessoryView = pasteControl() }
            return cell
        }
    }

    private func status() -> UITableViewCell {
        guard let current else {
            return row("Welcome to Phux Rune", detail: "Keep your terminals running on your computer. Open them here.",
                       icon: "desktopcomputer", tappable: false)
        }
        let cell = row("Paired with \(current)", detail: "This phone opens its terminals",
                       icon: "checkmark.circle.fill", tappable: false)
        cell.accessibilityIdentifier = "current-computer"
        return cell
    }

    /// Pasting through the system control never asks; tapping the row does, once.
    private func pasteControl() -> UIPasteControl {
        let options = UIPasteControl.Configuration()
        options.displayMode = .iconOnly
        let paste = UIPasteControl(configuration: options)
        // UITableView accessory views need their own nonzero bounds;
        // intrinsic content size alone leaves this control invisible.
        paste.frame = CGRect(x: 0, y: 0, width: 44, height: 44)
        paste.accessibilityLabel = "Paste pairing link"
        paste.target = self
        return paste
    }

    private func row(_ title: String, detail: String, icon: String, tint: UIColor? = nil,
                     tappable: Bool = true) -> UITableViewCell {
        let cell = UITableViewCell.titled(title, detail: detail, icon: icon, tint: tint ?? view.tintColor)
        cell.accessoryType = tappable ? .disclosureIndicator : .none
        cell.selectionStyle = tappable ? .default : .none
        return cell
    }

    override func tableView(_ tableView: UITableView, shouldHighlightRowAt indexPath: IndexPath) -> Bool {
        screen == .choices && indexPath.section > 0
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        guard screen == .choices else { return }
        switch (indexPath.section, indexPath.row) {
        case (1, 0): scan()
        case (1, 1): pasteLink()
        case (1, _): tailnet()
        case (2, _): advanced()
        default: break
        }
    }

    override func paste(itemProviders: [NSItemProvider]) {
        guard screen != .enrolling, let provider = itemProviders.first,
              provider.canLoadObject(ofClass: NSString.self) else { return }
        provider.loadObject(ofClass: NSString.self) { [weak self] value, _ in
            guard let text = value as? String else { return }
            Task { @MainActor [weak self] in self?.receive(text) }
        }
    }

    private func pasteLink() {
        let text = UIPasteboard.general.string?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !text.isEmpty else {
            show(PairingFlow.Problem(title: "There's no link to paste",
                message: "Copy the whole link that \(pairCommand) prints on your computer, then try again."))
            return
        }
        receive(text)
    }

    private func scan() {
        guard camera == nil else { return }
        camera = Task { [weak self] in
            defer { self?.camera = nil }
            do {
                try await PairingScanner.cameraReady()
                guard !Task.isCancelled, let self, self.view.window != nil else { return }
                let scanner = PairingScanner()
                scanner.received = { [weak self] in self?.receive($0) }
                scanner.failed = { [weak self] in self?.cameraFailed($0) }
                let navigation = UINavigationController(rootViewController: scanner)
                navigation.overrideUserInterfaceStyle = .dark
                self.present(navigation, animated: true)
            } catch { self?.cameraFailed(error.localizedDescription) }
        }
    }

    private func advanced() {
        let form = ConnectionForm(values: saved)
        form.connected = { [weak self] in self?.connected?($0) }
        navigationController?.pushViewController(form, animated: true)
    }

    private func tailnet() {
        let controller = TailscaleSetupController()
        controller.received = { [weak self] text in self?.receive(text) ?? false }
        navigationController?.pushViewController(controller, animated: true)
    }

    /// A failure must interrupt: felt, spoken and shown, not left in a footnote.
    private func interrupt(_ problem: PairingFlow.Problem) {
        UINotificationFeedbackGenerator().notificationOccurred(.error)
        UIAccessibility.post(notification: .announcement, argument: "\(problem.title). \(problem.message)")
        notice = show(problem) ? nil : problem
        tableView.reloadData()
    }

    private func cameraFailed(_ message: String) {
        let denied = PairingScanner.needsSettings
        show(PairingFlow.Problem(title: denied ? "Phux Rune can't use the camera" : "The camera can't scan right now",
                                 message: message), settings: denied)
    }

    /// Only while this screen is the one in front. Returns whether it was shown.
    @discardableResult
    private func show(_ problem: PairingFlow.Problem, settings: Bool = false) -> Bool {
        guard view.window != nil, presentedViewController == nil,
              navigationController.map({ $0.topViewController === self }) ?? true else { return false }
        let alert = UIAlertController.dark(title: problem.title, message: problem.message)
        if settings {
            alert.addAction(UIAlertAction(title: "Open Settings", style: .default) { _ in
                if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
            })
        }
        alert.addAction(UIAlertAction(title: "OK", style: .cancel))
        present(alert, animated: true)
        return true
    }
}
