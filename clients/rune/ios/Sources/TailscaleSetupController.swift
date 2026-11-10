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

    init() { super.init(style: .insetGrouped) }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
    isolated deinit { watching?.cancel(); pairing?.cancel() }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Find your computer"
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
                self?.snapshot = snapshot
                self?.refresh()
            }
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
        case 1: message ?? "Choose an online Mac or Linux computer. SSH gets its one-time Phux code securely; you won't type addresses, pins or tokens."
        default: "Keep the Tailscale app connected to the same tailnet. Rune's discovery node does not replace the phone's VPN. Scan a pairing QR instead if SSH is unavailable."
        }
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = UITableViewCell(style: .subtitle, reuseIdentifier: nil)
        cell.detailTextLabel?.numberOfLines = 0
        cell.textLabel?.numberOfLines = 2
        if indexPath.section == 0 { return loginCell(cell) }
        if indexPath.section == 2 { return tunnelCell(cell) }
        guard !snapshot.peers.isEmpty else {
            cell.textLabel?.text = "No computers available yet"
            cell.detailTextLabel?.text = "Sign in, allow any device approval, and check that your computers are online."
            cell.selectionStyle = .none
            return cell
        }
        let peer = snapshot.peers[indexPath.row]
        cell.textLabel?.text = selected == peer.id ? "Getting a code from \(peer.name)…" : peer.name
        cell.detailTextLabel?.text = peer.online ? peer.dnsName : "Offline"
        cell.imageView?.image = UIImage(systemName: "desktopcomputer")
        cell.accessoryType = peer.isPickable ? .disclosureIndicator : .none
        cell.contentView.alpha = peer.isPickable && pairing == nil ? 1 : 0.5
        return cell
    }

    private func loginCell(_ cell: UITableViewCell) -> UITableViewCell {
        cell.imageView?.image = UIImage(systemName: "network")
        switch snapshot.phase {
        case .starting:
            cell.textLabel?.text = "Starting secure discovery…"
        case .needsLogin(let url):
            cell.textLabel?.text = url == nil ? "Getting sign-in ready…" : "Sign in to Tailscale"
            cell.detailTextLabel?.text = "Use your existing Tailscale account"
            cell.accessoryType = url == nil ? .none : .disclosureIndicator
        case .needsApproval:
            cell.textLabel?.text = "Approve Rune in Tailscale"
            cell.detailTextLabel?.text = "Your tailnet requires administrator device approval"
            cell.accessoryType = .disclosureIndicator
        case .running:
            cell.textLabel?.text = "Signed in · manage discovery"
            cell.detailTextLabel?.text = snapshot.tailnetName
            cell.accessoryType = .disclosureIndicator
        case .stopped:
            cell.textLabel?.text = "Discovery could not start"
            cell.detailTextLabel?.text = "Sign out and retry, or scan a pairing QR"
            cell.accessoryType = .disclosureIndicator
        }
        return cell
    }

    private func tunnelCell(_ cell: UITableViewCell) -> UITableViewCell {
        let connected = TailnetPairing.hasSystemTunnel()
        cell.textLabel?.text = connected ? "Tailscale VPN connected" : "Connect the Tailscale app"
        cell.detailTextLabel?.text = connected ? "The selected computer must be on this same tailnet" : "Install or open Tailscale and turn its connection on"
        cell.imageView?.image = UIImage(systemName: connected ? "checkmark.shield" : "shield")
        cell.accessoryType = .disclosureIndicator
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        guard pairing == nil else { return }
        if indexPath.section == 0 { login(); return }
        if indexPath.section == 2 { openTailscale(); return }
        guard !snapshot.peers.isEmpty else { return }
        let peer = snapshot.peers[indexPath.row]
        guard peer.isPickable else { return }
        guard TailnetPairing.hasSystemTunnel() else {
            message = "Turn on the Tailscale app on this phone, then return and choose your computer."
            refresh()
            return
        }
        confirmSSH(peer)
    }

    private func login() {
        switch snapshot.phase {
        case .needsLogin(let url): if let url { openBrowser(url) }
        case .needsApproval: openBrowser(URL(string: "https://login.tailscale.com/admin/machines")!)
        case .running, .stopped: signOut()
        default: break
        }
    }

    private func signOut() {
        let alert = UIAlertController(title: "Discovery account", message: "Sign out of Rune's discovery node? This doesn't sign out the Tailscale VPN app or remove your Phux identity.", preferredStyle: .alert)
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        alert.addAction(UIAlertAction(title: "Sign out", style: .destructive) { [weak self] _ in
            guard let self else { return }
            let directory = self.directory
            let previous = self.watching
            previous?.cancel(); self.watching = nil
            Task { @MainActor [weak self] in
                await previous?.value
                await directory.signOut()
                self?.snapshot = TailnetSnapshot(phase: .starting)
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
                self.showPairingFailure(detail: "Rune received a pairing code but could not import it. No phone identity was enrolled. Try QR / paste or update Rune.",
                    peer: peer, username: username)
            }
        }
        if presentedViewController != nil { dismiss(animated: true, completion: deliver) }
        else { deliver() }
    }

    private func checkBanner(_ banner: String) {
        guard checkURL == nil, pairing != nil, let url = Self.sshCheckURL(banner) else { return }
        checkURL = url
        let alert = UIAlertController(title: "Approve Tailscale SSH",
            message: "Your SSH policy needs a fresh sign-in check. Approve it to continue pairing.", preferredStyle: .alert)
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
        return "Couldn't get a pairing code. Check SSH access and that Phux is running with QUIC, or scan the computer's phux pair --enroll QR."
    }

    private func openTailscale() { UIApplication.shared.open(URL(string: "https://tailscale.com/download/ios")!) }

    private func openBrowser(_ url: URL) {
        guard url.scheme == "https", url.host != nil, url.user == nil, url.password == nil else { return }
        present(SFSafariViewController(url: url), animated: true)
    }
}
