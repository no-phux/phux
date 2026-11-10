import UIKit
import SafariServices
import RuneTailnet

/// The embedded node is a directory, not a VPN. SSH and Rune use the
/// installed Tailscale tunnel; discovering an IP never establishes Phux trust.
@MainActor
final class TailscaleSetupController: UITableViewController {
    var received: ((String) -> Bool)?
    private let directory = EmbeddedTailnet(hostName: "phux-rune-ios")
    private var snapshot = TailnetSnapshot(phase: .starting)
    private var watching: Task<Void, Never>?
    private var pairing: Task<Void, Never>?
    private var message: String?
    private var checkURL: URL?
    private var selected: String?
    /// The page a tap opened, and the phase it was opened to get past.
    private weak var browser: SFSafariViewController?
    private var browserPhase: TailnetSnapshot.Phase?

    init() { super.init(style: .insetGrouped) }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
    isolated deinit { watching?.cancel(); pairing?.cancel() }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Find your computer"
        overrideUserInterfaceStyle = .dark
        tableView.accessibilityIdentifier = "tailnet-computers"
        NotificationCenter.default.addObserver(self, selector: #selector(refresh),
            name: UIApplication.didBecomeActiveNotification, object: nil)
        startWatching()
    }

    private func startWatching() {
        let directory = directory
        watching = Task { [weak self] in
            for await snapshot in directory.snapshots() {
                guard !Task.isCancelled else { break }
                self?.apply(snapshot)
            }
        }
    }

    private func apply(_ snapshot: TailnetSnapshot) {
        self.snapshot = snapshot
        closeFinishedBrowser()
        refresh()
    }

    /// Tailscale's page does not close itself once sign-in or approval lands.
    private func closeFinishedBrowser() {
        guard let browser, presentedViewController === browser, snapshot.phase != browserPhase else { return }
        switch snapshot.phase {
        case .running, .needsApproval:
            self.browser = nil
            dismiss(animated: true)
        default: break
        }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        if isMovingFromParent || navigationController?.isBeingDismissed == true {
            pairing?.cancel(); watching?.cancel()
        }
    }

    @objc private func refresh() { tableView.reloadData() }

    override func numberOfSections(in tableView: UITableView) -> Int { 3 }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        section == 1 ? max(1, snapshot.peers.count) : 1
    }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        ["Tailscale", snapshot.tailnetName ?? "Your computers", "This phone's connection"][section]
    }

    override func tableView(_ tableView: UITableView, titleForFooterInSection section: Int) -> String? {
        switch section {
        case 0: "Signing in adds a discovery-only Phux Rune device to your tailnet. Its state stays on this phone, outside backups. No logs are uploaded."
        case 1: message ?? "Choose an online Mac or Linux computer. Rune signs in to it over SSH once to get a pairing code; there's nothing else to type."
        default: "Keep the Tailscale app connected to the same tailnet. Rune's discovery does not replace the phone's VPN. If SSH is unavailable, scan a pairing QR instead."
        }
    }

    /// What tapping a row does, or nil for a row that only informs. One
    /// answer for highlighting and selection, so no row lights up and does nothing.
    private func action(at indexPath: IndexPath) -> (() -> Void)? {
        guard pairing == nil else { return nil }
        switch indexPath.section {
        case 0: return loginAction()
        case 2: return TailnetPairing.hasSystemTunnel() ? nil : { [weak self] in self?.openTailscale() }
        default:
            guard indexPath.row < snapshot.peers.count else { return nil }
            let peer = snapshot.peers[indexPath.row]
            return peer.isPickable ? { [weak self] in self?.choose(peer) } : nil
        }
    }

    private func loginAction() -> (() -> Void)? {
        switch snapshot.phase {
        case .starting, .needsLogin(loginURL: nil): return nil
        case .needsLogin(loginURL: let url?): return { [weak self] in self?.openBrowser(url) }
        case .needsApproval:
            return { [weak self] in self?.openBrowser(URL(string: "https://login.tailscale.com/admin/machines")!) }
        case .running, .stopped: return { [weak self] in self?.signOut() }
        }
    }

    /// What one row says, before it becomes a cell.
    private struct Row {
        var title = ""
        var detail: String?
        var icon: String?
        /// Waiting on something: a spinner where the chevron would be.
        var busy = false
        /// Listed but not offered, with `detail` saying why.
        var dimmed = false
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let row = switch indexPath.section {
        case 0: loginRow()
        case 2: tunnelRow()
        default: peerRow(indexPath.row)
        }
        let cell = UITableViewCell.titled(row.title, detail: row.detail, icon: row.icon,
                                          tint: view.tintColor, dimmed: row.dimmed)
        let tappable = action(at: indexPath) != nil
        cell.selectionStyle = tappable ? .default : .none
        if row.busy {
            let spinner = UIActivityIndicatorView(style: .medium)
            spinner.startAnimating()
            cell.accessoryView = spinner
        } else if tappable, navigates(indexPath) {
            cell.accessoryType = .disclosureIndicator
        }
        return cell
    }

    /// A chevron promises another screen; signing out only asks a question.
    private func navigates(_ indexPath: IndexPath) -> Bool {
        guard indexPath.section == 0 else { return true }
        switch snapshot.phase {
        case .running, .stopped: return false
        default: return true
        }
    }

    private func peerRow(_ index: Int) -> Row {
        guard index < snapshot.peers.count else {
            return Row(title: "No computers yet",
                       detail: "Sign in above, then check that your computers are online in Tailscale.")
        }
        let peer = snapshot.peers[index]
        var row = Row(title: peer.name, detail: peer.dnsName, icon: "desktopcomputer")
        if selected == peer.id {
            row.title = "Getting a code from \(peer.name)…"
            row.busy = true
        } else if !peer.canRunPhux {
            row.detail = "Phux runs on Mac and Linux computers"
            row.dimmed = true
        } else if !peer.online {
            row.detail = "Offline"
            row.dimmed = true
        } else if !peer.isPickable {
            row.detail = "No Tailscale address yet"
            row.dimmed = true
        } else {
            row.dimmed = pairing != nil
        }
        return row
    }

    private func loginRow() -> Row {
        var row = Row(icon: "network")
        switch snapshot.phase {
        case .starting:
            row.title = "Starting Tailscale discovery…"
            row.busy = true
        case .needsLogin(let url):
            row.title = url == nil ? "Getting sign-in ready…" : "Sign in to Tailscale"
            row.detail = "Use your existing Tailscale account"
            row.busy = url == nil
        case .needsApproval:
            row.title = "Approve this phone in Tailscale"
            row.detail = "Your tailnet's admin approves new devices"
        case .running:
            row.title = "Sign out of discovery"
            row.detail = snapshot.tailnetName.map { "Signed in to \($0)" } ?? "Signed in"
        case .stopped:
            row.title = "Discovery couldn't start"
            row.detail = "Tap to sign out and start again, or scan a pairing QR instead"
        }
        return row
    }

    private func tunnelRow() -> Row {
        TailnetPairing.hasSystemTunnel()
            ? Row(title: "Tailscale is on", detail: "Your computer must be on this same tailnet", icon: "checkmark.shield")
            : Row(title: "Turn on Tailscale", detail: "Install or open the Tailscale app and connect it", icon: "shield.slash")
    }

    override func tableView(_ tableView: UITableView, shouldHighlightRowAt indexPath: IndexPath) -> Bool {
        action(at: indexPath) != nil
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        action(at: indexPath)?()
    }

    private func choose(_ peer: TailnetPeer) {
        guard TailnetPairing.hasSystemTunnel() else {
            let alert = UIAlertController.dark(title: "Turn on Tailscale first",
                message: "Open the Tailscale app on this phone and connect it, then choose \(peer.name) again.")
            alert.addAction(UIAlertAction(title: "OK", style: .cancel))
            present(alert, animated: true)
            return
        }
        confirmSSH(peer)
    }

    private func signOut() {
        let alert = UIAlertController.dark(title: "Sign out of Tailscale discovery?",
            message: "Rune stops listing your computers. The Tailscale app stays signed in, and this phone stays paired.")
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        alert.addAction(UIAlertAction(title: "Sign Out", style: .destructive) { [weak self] _ in
            guard let self else { return }
            let directory = self.directory
            let previous = self.watching
            previous?.cancel(); self.watching = nil
            Task { @MainActor [weak self] in
                await previous?.value
                await directory.signOut()
                self?.apply(TailnetSnapshot(phase: .starting))
                self?.startWatching()
            }
        })
        present(alert, animated: true)
    }

    private func confirmSSH(_ peer: TailnetPeer, username: String? = nil) {
        let alert = SSHAccountPrompt.make(peer: peer, username: username ?? SSHAccountPrompt.saved(for: peer)) {
            [weak self] user, password in self?.pair(peer, username: user, password: password)
        }
        present(alert, animated: true)
    }

    private func pair(_ peer: TailnetPeer, username: String, password: String?) {
        guard pairing == nil else { return }
        selected = peer.id; checkURL = nil; message = nil
        OnboardingStatus.record(.connecting, peer: peer)
        pairing = Task { [weak self] in
            defer {
                self?.pairing = nil
                self?.selected = nil
                self?.refresh()
            }
            do {
                let link = try await TailnetPairing.bootstrap(peer: peer, username: username,
                    password: password, onBanner: { [weak self] banner in
                        Task { @MainActor [weak self] in self?.checkBanner(banner) }
                    })
                guard !Task.isCancelled, let self else { return }
                SSHAccountPrompt.remember(username, for: peer)
                OnboardingStatus.record(.codeReceived, peer: peer)
                self.finishPairing(link, peer: peer, username: username)
            } catch {
                guard !Task.isCancelled else { return }
                OnboardingStatus.record(.failed, peer: peer, failure: error)
                self?.showPairingFailure(error, peer: peer, username: username)
            }
        }
        refresh()
    }

    private func showPairingFailure(_ error: Error, peer: TailnetPeer, username: String) {
        showPairingFailure(detail: Self.pairingFailure(error), peer: peer, username: username)
    }

    private func showPairingFailure(detail: String, peer: TailnetPeer, username: String) {
        message = detail
        UINotificationFeedbackGenerator().notificationOccurred(.error)
        let alert = SSHAccountPrompt.failure(peer: peer, message: detail, retry: { [weak self] in
            self?.confirmSSH(peer, username: username)
        }, back: { [weak self] in
            self?.navigationController?.popToRootViewController(animated: true)
        })
        let show = { [weak self] in
            guard let self, self.view.window != nil else { return }
            self.present(alert, animated: true)
        }
        if presentedViewController != nil { dismiss(animated: true, completion: show) }
        else { show() }
    }

    private func finishPairing(_ link: String, peer: TailnetPeer, username: String) {
        // Approval can finish while Safari covers this controller. Preserve
        // the code and return to explicit peer confirmation, not a hidden UI.
        let deliver = { [weak self] in
            guard let self else { return }
            if self.received?(link) == true {
                self.navigationController?.popViewController(animated: true)
            } else {
                OnboardingStatus.record(.importFailed, peer: peer)
                self.showPairingFailure(detail: "The computer sent a pairing code this version of Rune can't read. Nothing was changed. Update Rune, or scan the computer's QR code instead.",
                    peer: peer, username: username)
            }
        }
        if presentedViewController != nil { dismiss(animated: true, completion: deliver) }
        else { deliver() }
    }

    private func checkBanner(_ banner: String) {
        guard checkURL == nil, pairing != nil, let url = Self.sshCheckURL(banner) else { return }
        checkURL = url
        let alert = UIAlertController.dark(title: "Tailscale needs to check it's you",
            message: "Your tailnet asks for a fresh sign-in before SSH. Approve it to continue pairing.")
        alert.addAction(UIAlertAction(title: "Cancel setup", style: .cancel) { [weak self] _ in self?.pairing?.cancel() })
        alert.addAction(UIAlertAction(title: "Approve", style: .default) { [weak self] _ in self?.openBrowser(url) })
        present(alert, animated: true)
    }

    static func sshCheckURL(_ banner: String) -> URL? {
        guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else { return nil }
        let matches = detector.matches(in: banner, range: NSRange(banner.startIndex..., in: banner))
        return matches.compactMap(\.url).first {
            $0.scheme == "https" && $0.host == "login.tailscale.com"
                && $0.user == nil && $0.password == nil && ($0.port == nil || $0.port == 443)
        }
    }

    static func pairingFailure(_ error: Error) -> String {
        // These enums have fixed, credential-free diagnostics. Arbitrary
        // SSH/NIO errors and remote output never cross into the UI.
        if let error = error as? TailnetPairingError { return error.localizedDescription }
        if let error = error as? SSHFailure {
            if case .authenticationFailed = error {
                return "The computer rejected that login. Use its local account name and computer login password—not your Tailscale or Apple ID login. You can retry or use QR / paste without SSH."
            }
            return error.localizedDescription
        }
        return "Couldn't get a pairing code. Check SSH access and that Phux is running with QUIC, or scan the computer's \(pairCommand) QR."
    }

    private func openTailscale() { UIApplication.shared.open(URL(string: "https://tailscale.com/download/ios")!) }

    private func openBrowser(_ url: URL) {
        guard url.scheme == "https", url.host != nil, url.user == nil, url.password == nil else { return }
        let page = SFSafariViewController(url: url)
        browser = page
        browserPhase = snapshot.phase
        present(page, animated: true)
    }
}
