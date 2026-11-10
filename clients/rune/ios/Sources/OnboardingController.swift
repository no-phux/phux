import UIKit

/// First run is Scan, Paste or Find a computer—not a credential form.
@MainActor
final class OnboardingController: UITableViewController {
    var connected: (([String: Any]) -> Void)?
    var closed: (() -> Void)?
    private let saved: [String: Any]?
    let flow = PairingFlow()
    private var camera: Task<Void, Never>?

    init(values: [String: Any]?) {
        saved = values
        super.init(style: .insetGrouped)
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
    isolated deinit { camera?.cancel() }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Connect your computer"
        navigationItem.leftBarButtonItem = UIBarButtonItem(systemItem: .cancel,
            primaryAction: UIAction { [weak self] _ in self?.cancel() })
        pasteConfiguration = UIPasteConfiguration(forAccepting: NSString.self)
        flow.changed = { [weak self] in self?.refresh() }
        flow.connected = { [weak self] values in self?.connected?(values) }
        tableView.accessibilityIdentifier = "rune-onboarding"
    }

    func receive(_ link: String) {
        loadViewIfNeeded()
        flow.receive(link)
    }

    private func refresh() {
        if case .enrolling = flow.stage { isModalInPresentation = true }
        else { isModalInPresentation = false }
        tableView.reloadData()
    }

    private func cancel() {
        camera?.cancel(); camera = nil
        flow.cancel()
        dismiss(animated: true, completion: closed)
    }

    override func numberOfSections(in tableView: UITableView) -> Int { 3 }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        section == 0 ? 1 : section == 1 ? 3 : 1
    }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        ["Your terminals, wherever you are", "Choose how to connect", "More options"][section]
    }

    override func tableView(_ tableView: UITableView, titleForFooterInSection section: Int) -> String? {
        switch section {
        case 0: flow.message ?? "No addresses, pins or tokens to type. Pairing gives this phone its own key."
        case 1: "On a computer, phux pair --enroll prints a pairing code. Scan it, or paste its whole link. Tailscale can find your computers and get the code over SSH instead."
        default: "Advanced setup is only for troubleshooting or a custom endpoint."
        }
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = UITableViewCell(style: .subtitle, reuseIdentifier: nil)
        cell.detailTextLabel?.numberOfLines = 0
        cell.textLabel?.numberOfLines = 2
        if indexPath.section == 0 { return statusCell(cell) }
        if indexPath.section == 2 {
            configure(cell, "Advanced setup", detail: "Manual connection settings", icon: "slider.horizontal.3")
            return cell
        }
        let choices = [("Scan pairing QR", "Use this phone's camera", "qrcode.viewfinder"),
                       ("Paste pairing link", "Paste the whole link—never individual credentials", "doc.on.clipboard"),
                       ("Find with Tailscale", "Sign in and choose a computer", "network")]
        let choice = choices[indexPath.row]
        configure(cell, choice.0, detail: choice.1, icon: choice.2)
        cell.accessibilityIdentifier = ["scan-pairing", "paste-pairing", "find-tailnet"][indexPath.row]
        if busy { cell.selectionStyle = .none; cell.contentView.alpha = 0.5 }
        if indexPath.row == 1 {
            let paste = UIPasteControl()
            paste.target = self
            cell.accessoryView = paste
        }
        return cell
    }

    private var busy: Bool {
        if case .enrolling = flow.stage { return true }
        return false
    }

    private func configure(_ cell: UITableViewCell, _ title: String, detail: String, icon: String) {
        cell.textLabel?.text = title
        cell.detailTextLabel?.text = detail
        cell.imageView?.image = UIImage(systemName: icon)
        cell.imageView?.tintColor = view.tintColor
        cell.accessoryType = .disclosureIndicator
    }

    private func statusCell(_ cell: UITableViewCell) -> UITableViewCell {
        switch flow.stage {
        case .welcome:
            configure(cell, saved == nil ? "Welcome to Phux Rune" : "Add or change a computer",
                      detail: "Keep your terminals running on your computer. Open them here.", icon: "desktopcomputer")
            cell.accessoryType = .none; cell.selectionStyle = .none
        case .review(let invite):
            configure(cell, "Pair & connect to \(invite.name)",
                      detail: "\(invite.peer)\nOnly continue if this is your computer. The code supplies its verified trust pins.", icon: "checkmark.shield")
            cell.accessibilityIdentifier = "confirm-pairing"
        case .enrolling(let name):
            configure(cell, "Pairing with \(name)…", detail: "Creating this phone's identity securely", icon: "lock.shield")
            let spinner = UIActivityIndicatorView(style: .medium); spinner.startAnimating()
            cell.accessoryView = spinner; cell.selectionStyle = .none
        case .finish:
            configure(cell, "Retry saving & connect", detail: "Your pairing code won't be used again", icon: "key")
        }
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        guard !busy else { return }
        if indexPath.section == 0 { flow.confirm(); return }
        if indexPath.section == 2 { advanced(); return }
        switch indexPath.row {
        case 0: scan()
        case 1: pasteHelp()
        default: tailnet()
        }
    }

    override func paste(itemProviders: [NSItemProvider]) {
        guard !busy, let provider = itemProviders.first,
              provider.canLoadObject(ofClass: NSString.self) else { return }
        provider.loadObject(ofClass: NSString.self) { [weak self] value, _ in
            guard let text = value as? String else { return }
            Task { @MainActor [weak self] in self?.receive(text) }
        }
    }

    private func pasteHelp() {
        let alert = UIAlertController(title: "Paste a pairing link",
            message: "Use the Paste button on this row to paste the complete link from phux pair --enroll. You don't need to copy its individual fields.", preferredStyle: .alert)
        alert.addAction(UIAlertAction(title: "OK", style: .default))
        present(alert, animated: true)
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
                scanner.failed = { [weak self] in self?.show($0) }
                self.present(UINavigationController(rootViewController: scanner), animated: true)
            } catch { self?.show(error.localizedDescription) }
        }
    }

    private func advanced() {
        let form = ConnectionForm(values: saved)
        form.connected = { [weak self] in self?.connected?($0) }
        navigationController?.pushViewController(form, animated: true)
    }

    private func tailnet() {
        let controller = TailscaleSetupController()
        controller.received = { [weak self] text in self?.receive(text) }
        navigationController?.pushViewController(controller, animated: true)
    }

    private func show(_ message: String) {
        guard view.window != nil else { return }
        let alert = UIAlertController(title: "Pairing", message: message, preferredStyle: .alert)
        if PairingScanner.needsSettings {
            alert.addAction(UIAlertAction(title: "Open Settings", style: .default) { _ in
                if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
            })
        }
        alert.addAction(UIAlertAction(title: "OK", style: .default))
        present(alert, animated: true)
    }
}
