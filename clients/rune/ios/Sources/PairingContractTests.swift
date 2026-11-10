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
        try await confirmationAndCommit()
        try await saveRetry()
        try await cancelledLateCompletion()
        return ["pairing-import", "pairing-confirmation", "pairing-save-retry", "pairing-cancelled-identity",
                "tailnet-account", "tailnet-errors"]
    }

    private static func tailnetAccountsAndFailures() throws {
        var peer = TailnetPeer(id: "account-fixture", name: "Computer", dnsName: "computer.example.ts.net",
            platform: .macOS, online: true, address: "100.64.1.2", owner: "not-a-local-account@example.com")
        let fresh = SSHAccountPrompt.make(peer: peer, username: nil) { _, _ in }
        try require(fresh.textFields?.first?.text?.isEmpty != false, "never infer a computer account from a Tailscale email")
        try require(fresh.message?.contains("not your Tailscale") == true, "explain which account/password are needed")
        try require(fresh.textFields?.last?.isSecureTextEntry == true, "ordinary SSH password entry is protected")
        let retry = SSHAccountPrompt.make(peer: peer, username: "local-user") { _, _ in }
        try require(retry.textFields?.first?.text == "local-user", "retry retains the explicit account name")
        try require(retry.textFields?.last?.text?.isEmpty != false, "retry never retains a password")
        peer.sshHostKeys = ["ssh-ed25519 fixture"]
        let pinned = SSHAccountPrompt.make(peer: peer, username: nil) { _, _ in }
        try require(pinned.textFields?.count == 1, "Tailscale SSH does not ask for a computer password")
        let failure = SSHAccountPrompt.failure(peer: peer, message: "safe", retry: {}, back: {})
        try require(failure.actions.map(\.title) == ["Try again", "Back to QR / paste"], "a failed login has visible recovery actions")
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
        try require(invite.configuration["ticket"] == nil, "ticket is never a saved configuration field")
        for invalid in [link + "\0ignored", String(repeating: "x", count: 16_385), "https://elsewhere.invalid/SECRET-CREDENTIAL"] {
            do {
                _ = try PairingInvite(link: invalid)
                throw Failure(message: "invalid pairing was accepted")
            } catch let error as Failure { throw error }
            catch { try require(!error.localizedDescription.contains("SECRET-CREDENTIAL"), "import errors redact content") }
        }
        let welcome = OnboardingController(values: nil)
        welcome.loadViewIfNeeded()
        try require(welcome.tableView.numberOfRows(inSection: 1) == 3, "Scan, Paste and Tailscale are primary")
        let paste = welcome.tableView(welcome.tableView, cellForRowAt: IndexPath(row: 1, section: 1)).accessoryView
        try require(paste is UIPasteControl && (paste?.bounds.width ?? 0) >= 44 && (paste?.bounds.height ?? 0) >= 44,
                    "the explicit-consent Paste control must be visible and tappable")
        welcome.receive(link)
        guard case .review = welcome.flow.stage else { throw Failure(message: "import must stage a confirmation") }
        welcome.flow.cancel()
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
        }, save: { _ in saved += 1 }, discard: { _ in discarded += 1 })
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
        }, save: { _ in
            saves += 1
            if saves == 1 { throw Failure(message: "Keychain locked fixture") }
        }, discard: { _ in })
        flow.connected = { _ in connected += 1 }
        flow.receive(link); flow.confirm()
        await flow.qualificationOperation?.value
        guard case .finish = flow.stage else { throw Failure(message: "save failure retains the issued identity") }
        flow.confirm()
        try require(enrolled == 1 && saves == 2 && connected == 1, "save retry must not spend the ticket twice")
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
            }, save: { _ in saved += 1 }, discard: { _ in discarded += 1 })
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
