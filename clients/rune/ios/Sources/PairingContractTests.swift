import UIKit
import RuneTailnet

#if targetEnvironment(simulator)
@MainActor
enum PairingContractTests {
    private struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static func require(_ value: @autoclosure () -> Bool, _ message: String) throws {
        if !value() { throw Failure(message: message) }
    }

    static let link = "https://phux.sh/connect?url=wss://computer.example:443&quic=quic://computer.example:8788&name=My%20Mac&fp="
        + String(repeating: "a", count: 64) + "&ca=sha256:" + String(repeating: "b", count: 64)
        + "&enroll=" + String(repeating: "c", count: 64) + "&token=" + String(repeating: "d", count: 64)

    static func run() async throws -> [String] {
        try parsingAndFirstRun()
        try tailnetAccountsAndFailures()
        try tailnetProducerImport()
        try await confirmationAndCommit()
        try await saveRetry()
        try await cancelledLateCompletion()
        return ["pairing-import", "pairing-confirmation", "pairing-save-retry", "pairing-cancelled-identity",
                "tailnet-account", "tailnet-errors", "tailnet-import"]
    }

    private static func tailnetProducerImport() throws {
        let peer = TailnetPeer(id: "producer-fixture", name: "My Mac", dnsName: "computer.example.ts.net",
            platform: .macOS, online: true, address: "100.64.1.2")
        var document: [String: Any] = ["schema_version": 1, "token": String(repeating: "d", count: 64),
            "cert_fingerprint": String(repeating: "a", count: 64),
            "ca_fingerprint": "sha256:" + String(repeating: "b", count: 64),
            "enrollment_ticket": String(repeating: "c", count: 64), "quic_addr": "0.0.0.0:8788",
            "connect_link": link.replacingOccurrences(of: "computer.example", with: peer.dnsName)]
        for hasWebSocket in [true, false] {
            if !hasWebSocket { document.removeValue(forKey: "connect_link") }
            let emitted = try TailnetPairing.pairingLink(from: JSONSerialization.data(withJSONObject: document), peer: peer)
            let imported = try PairingInvite(link: emitted) // Actual Swift producer -> C -> Rust consumer.
            try require(imported.configuration["endpoint"] as? String == "quic://100.64.1.2:8788",
                        "the exact tailnet pairing output must import without a WebSocket fallback")
            try require(imported.ticket == String(repeating: "c", count: 64), "producer/consumer preserves the one-use ticket")
        }
    }

    private static func tailnetAccountsAndFailures() throws {
        var peer = TailnetPeer(id: "account-fixture", name: "Computer", dnsName: "computer.example.ts.net",
            platform: .macOS, online: true, address: "100.64.1.2", owner: "not-a-local-account@example.com")
        let fresh = SSHAccountPrompt.make(peer: peer, username: nil) { _, _ in }
        try require(fresh.textFields?.first?.text?.isEmpty != false, "never infer a computer account from a Tailscale email")
        try require(fresh.message?.contains("not your Tailscale") == true, "explain which account/password are needed")
        try require(fresh.textFields?.last?.isSecureTextEntry == true, "ordinary SSH password entry is protected")
        try require(fresh.actions.last?.isEnabled == false && fresh.preferredAction === fresh.actions.last,
                    "a blank account name cannot be sent")
        fresh.textFields?.first?.text = "local-user"
        fresh.textFields?.first?.sendActions(for: .editingChanged)
        try require(fresh.actions.last?.isEnabled == true, "typing an account name enables the request")
        let retry = SSHAccountPrompt.make(peer: peer, username: "local-user") { _, _ in }
        try require(retry.textFields?.first?.text == "local-user" && retry.actions.last?.isEnabled == true,
                    "retry retains the explicit account name")
        try require(retry.textFields?.last?.text?.isEmpty != false, "retry never retains a password")
        peer.sshHostKeys = ["ssh-ed25519 fixture"]
        let pinned = SSHAccountPrompt.make(peer: peer, username: nil) { _, _ in }
        try require(pinned.textFields?.count == 1, "Tailscale SSH does not ask for a computer password")
        let failure = SSHAccountPrompt.failure(peer: peer, message: "safe", retry: {}, back: {})
        try require(failure.actions.map(\.title) == ["Try Again", "Scan or Paste Instead", "OK"]
                    && failure.actions.last?.style == .cancel, "a failed login can be retried, routed around or simply read")
        let error = SSHFailure.authenticationFailed(passwordAccepted: true)
        try require(OnboardingStatus.code(error) == "ssh_authentication", "diagnose authentication separately from discovery")
        try require(OnboardingStatus.code(SSHFailure.unreachable("SECRET-CREDENTIAL")) == "ssh_unreachable", "diagnostics never persist raw errors")
        try require(TailscaleSetupController.pairingFailure(error).contains("local account"), "authentication errors name the correct credential")
        OnboardingStatus.record(.failed, peer: peer, failure: SSHFailure.unreachable("SECRET-CREDENTIAL"))
        let documents = try FileManager.default.url(for: .documentDirectory, in: .userDomainMask, appropriateFor: nil, create: false)
        let bytes = try Data(contentsOf: documents.appendingPathComponent("rune-onboarding-status.json"))
        let report = try JSONSerialization.jsonObject(with: bytes) as? [String: Any]
        try require(report?["failure"] as? String == "ssh_unreachable", "support evidence contains the typed failure code")
        try require(Set(report?.keys.map { $0 } ?? []) == ["schema", "phase", "peer", "time", "failure"], "support evidence is a strict public-field whitelist")
        try require(!String(decoding: bytes, as: UTF8.self).contains("SECRET-CREDENTIAL"), "raw errors never reach support evidence")
    }

    private static func parsingAndFirstRun() throws {
        let invite = try PairingInvite(link: link)
        try require(invite.configuration["endpoint"] as? String == "quic://computer.example:8788", "QR prefers QUIC")
        try require(invite.name == "My Mac", "QR preserves its decoded name")
        let unnamed = try PairingInvite(link: link.replacingOccurrences(of: "&name=My%20Mac", with: ""))
        try require(unnamed.name == "computer.example" && unnamed.host == "computer.example",
                    "a link without a name is shown by its host, never by its endpoint")
        try require(invite.configuration["ticket"] == nil, "ticket is never a saved configuration field")
        for invalid in [link + "\0ignored", String(repeating: "x", count: 16_385), "https://elsewhere.invalid/SECRET-CREDENTIAL"] {
            do {
                _ = try PairingInvite(link: invalid)
                throw Failure(message: "invalid pairing was accepted")
            } catch let error as Failure { throw error }
            catch { try require(!error.localizedDescription.contains("SECRET-CREDENTIAL"), "import errors redact content") }
        }
        // The real parser and screen; an enrollment that only ever waits.
        let welcome = OnboardingController(values: nil, flow: PairingFlow(enroll: { _ in
            try await Task.sleep(for: .seconds(3600))
            throw CancellationError()
        }, save: { _, _ in }, discard: { _ in }))
        welcome.loadViewIfNeeded()
        try require(welcome.tableView.numberOfRows(inSection: 1) == 3, "Scan, Paste and Tailscale are primary")
        let paste = welcome.tableView(welcome.tableView, cellForRowAt: IndexPath(row: 1, section: 1)).accessoryView
        try require(paste is UIPasteControl && (paste?.bounds.width ?? 0) >= 44 && (paste?.bounds.height ?? 0) >= 44,
                    "the explicit-consent Paste control must be visible and tappable")
        try require(!welcome.receive("not-a-pairing-link/SECRET-CREDENTIAL"), "rejected imports must not acknowledge success")
        let notice = welcome.tableView(welcome.tableView, cellForRowAt: IndexPath(row: 1, section: 0))
        try require(welcome.tableView.numberOfRows(inSection: 0) == 2 && notice.accessibilityIdentifier == "pairing-notice",
                    "a rejection no alert could deliver stays in the list, not in a footer")
        try require(welcome.flow.message?.isEmpty == false && welcome.flow.message?.contains("SECRET-CREDENTIAL") == false,
                    "a rejected link is explained without echoing it")
        try require(welcome.receive(link), "accepted imports acknowledge peer review")
        guard case .review = welcome.flow.stage else { throw Failure(message: "import must stage a confirmation") }
        try require(welcome.numberOfSections(in: welcome.tableView) == 1 && welcome.tableView.numberOfRows(inSection: 0) == 2,
                    "a staged pairing shows only Pair and Cancel, never other ways to connect")
        let question = welcome.tableView(welcome.tableView, viewForHeaderInSection: 0) as? OnboardingHero
        try require(question?.title.text == "Pair with My Mac?" && question?.detail.text?.contains("computer.example") == true,
                    "the confirmation names the computer and its host")
        try require(question?.detail.text?.contains("8788") == false && question?.detail.text?.contains("quic") == false,
                    "the confirmation shows a host, not an endpoint")
        let confirmation = welcome.tableView(welcome.tableView, cellForRowAt: IndexPath(row: 0, section: 0)) as? PairingActionCell
        let button = confirmation?.button
        try require(confirmation?.accessibilityIdentifier == "confirm-pairing" && button?.isEnabled == true
                    && button?.configuration?.title == "Pair & Connect"
                    && button?.configuration?.baseBackgroundColor == welcome.view.tintColor,
                    "peer confirmation must be a filled, enabled button")
        let dismissal = welcome.tableView(welcome.tableView, cellForRowAt: IndexPath(row: 1, section: 0)) as? PairingActionCell
        try require(dismissal?.button.configuration?.title == "Cancel" && dismissal?.button.configuration?.baseBackgroundColor == nil,
                    "only the confirmation is filled")
        try require(!welcome.receive("not-a-pairing-link"), "rejected imports must not acknowledge success")
        guard case .review = welcome.flow.stage else { throw Failure(message: "a rejected link must not discard the staged computer") }
        button?.sendActions(for: .primaryActionTriggered)
        guard case .enrolling = welcome.flow.stage else { throw Failure(message: "the button is the confirmation") }
        try require(welcome.numberOfSections(in: welcome.tableView) == 1 && welcome.tableView.numberOfRows(inSection: 0) == 0
                    && welcome.isModalInPresentation, "enrolling shows progress alone and cannot be swiped away")
        welcome.flow.cancel()
        try require(welcome.numberOfSections(in: welcome.tableView) == 3, "cancelling returns to the ways to connect")
        let check = "https://login.tailscale.com/check/v1.3?ts_session_id=abc.def+ghi"
        try require(TailscaleSetupController.sshCheckURL("Please approve: \(check)\n")?.absoluteString == check,
                    "SSH check links preserve dots and query punctuation")
        for invalid in ["https://login.tailscale.com.evil.invalid/check/secret",
                        "https://login.tailscale.com@evil.invalid/check/secret",
                        "http://login.tailscale.com/check/secret", "https://login.tailscale.com:444/check/secret"] {
            try require(TailscaleSetupController.sshCheckURL(invalid) == nil, "SSH check requires the exact HTTPS login host")
        }
        let secretFailure = Failure(message: "remote-SECRET-CREDENTIAL")
        try require(!TailscaleSetupController.pairingFailure(secretFailure).contains("SECRET-CREDENTIAL"),
                    "unknown SSH diagnostics must not echo remote output")
        try require(TailscaleSetupController.pairingFailure(TailnetPairingError.phuxNotInstalled).contains("Install phux"),
                    "SSH failures explain the actual safe recovery")
    }

    private static func confirmationAndCommit() async throws {
        var enrolled = 0, saved = 0, discarded = 0, connected = 0
        let flow = PairingFlow(enroll: { invite in
            enrolled += 1
            return invite.configuration.merging(["key_id": "fixture", "chain_pem": "public-chain"]) { _, new in new }
        }, save: { _, name in if name == "My Mac" { saved += 1 } }, discard: { _ in discarded += 1 })
        flow.connected = { _ in connected += 1 }
        flow.receive(link)
        try require(enrolled == 0 && saved == 0, "scanning never enrolls or overwrites a working identity")
        flow.confirm()
        let operation = flow.qualificationOperation
        flow.confirm()
        await operation?.value
        try require(enrolled == 1 && saved == 1 && connected == 1, "one confirmation enrolls, saves and connects once")
        flow.cancel()
        try require(discarded == 0, "a saved identity is not discarded on sheet teardown")
    }

    private static func saveRetry() async throws {
        var enrolled = 0, saves = 0, connected = 0
        let flow = PairingFlow(enroll: { invite in
            enrolled += 1
            return invite.configuration
        }, save: { _, _ in
            saves += 1
            if saves == 1 { throw Failure(message: "Keychain locked fixture SECRET-CREDENTIAL") }
        }, discard: { _ in })
        var problems: [PairingFlow.Problem] = []
        flow.connected = { _ in connected += 1 }
        flow.failed = { problems.append($0) }
        flow.receive(link); flow.confirm()
        await flow.qualificationOperation?.value
        guard case .finish = flow.stage else { throw Failure(message: "save failure retains the issued identity") }
        try require(problems.count == 1 && problems[0].message.contains("don't need another code")
                    && !problems[0].message.contains("SECRET-CREDENTIAL"), "a failed save interrupts once, in plain words")
        flow.confirm()
        try require(enrolled == 1 && saves == 2 && connected == 1, "save retry must not spend the ticket twice")
        try await enrollmentFailures()
    }

    /// Each class of failure gets its own instruction and support code, and
    /// none of them repeats the error it was read from.
    private static func enrollmentFailures() async throws {
        let cases: [(String, PairingFailure, String)] = [
            ("studio did not answer (SECRET-CREDENTIAL); check that it is up", .unreachable, "enroll_unreachable"),
            ("studio: could not resolve SECRET-CREDENTIAL: no such host", .unreachable, "enroll_unreachable"),
            ("studio refused the pairing token (enrollment refused: SECRET-CREDENTIAL)", .codeRejected, "enroll_code_rejected"),
            ("Could not create the device enrollment key. SECRET-CREDENTIAL", .deviceKey, "enroll_device_key"),
            ("SECRET-CREDENTIAL", .other, "unknown")]
        var titles = Set<String>()
        for (raw, expected, code) in cases {
            var saved = 0
            let flow = PairingFlow(enroll: { _ in throw Failure(message: raw) }, save: { _, _ in saved += 1 }, discard: { _ in })
            var problems: [PairingFlow.Problem] = []
            flow.failed = { problems.append($0) }
            flow.receive(link); flow.confirm()
            await flow.qualificationOperation?.value
            try require(PairingFailure(Failure(message: raw)) == expected && OnboardingStatus.code(expected) == code,
                        "enrollment failures are classified for the person and for support")
            try require(problems.count == 1 && saved == 0 && flow.message == problems[0].message
                        && !(problems[0].title + problems[0].message).contains("SECRET-CREDENTIAL"),
                        "a failed enrollment interrupts once and never echoes the raw error")
            titles.insert(problems[0].title)
            if case .review = flow.stage { try require(expected.keepsCode, "only an unspent code is offered again") }
            else { try require(!expected.keepsCode, "an unspent code stays ready to retry") }
        }
        try require(titles.count == 4, "each class of failure reads differently")
    }

    private static func cancelledLateCompletion() async throws {
        var delivery: CheckedContinuation<[String: Any], Never>?
        var flow: PairingFlow?
        var saved = 0, discarded = 0
        await withCheckedContinuation { started in
            let candidate = PairingFlow(enroll: { _ in
                await withCheckedContinuation { continuation in
                    delivery = continuation
                    started.resume()
                }
            }, save: { _, _ in saved += 1 }, discard: { _ in discarded += 1 })
            flow = candidate
            candidate.receive(link); candidate.confirm()
        }
        guard let flow, let delivery else { throw Failure(message: "enrollment fixture did not start") }
        let operation = flow.qualificationOperation
        flow.cancel()
        delivery.resume(returning: ["key_id": "cancelled-fixture", "chain_pem": "public-chain"])
        await operation?.value
        try require(saved == 0 && discarded == 1, "late completion deletes its staged key without changing saved credentials")
    }
}
#endif
