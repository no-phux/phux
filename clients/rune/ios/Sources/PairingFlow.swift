import Foundation

/// The command a person runs on their computer, joined so it never wraps
/// mid-flag with "--" ending one line and "enroll" starting the next.
let pairCommand = "phux\u{00A0}pair\u{00A0}--\u{2060}enroll"

@MainActor
struct PairingInvite {
    let configuration: [String: Any]
    let ticket: String
    let name: String
    let peer: String

    init(link: String) throws {
        // withCString must not silently truncate an imported credential.
        guard link.utf8.count <= 16_384, !link.utf8.contains(0) else {
            throw NativeFailure.message("This pairing link is invalid or too large.")
        }
        let value = try link.withCString { try NativeBridge.object(phux_rune_ios_pairing_import($0)) }
        guard let configuration = value["configuration"] as? [String: Any],
              let ticket = value["ticket"] as? String,
              let peer = value["peer"] as? String else {
            throw NativeFailure.message("This pairing link is incomplete. Make a fresh code with \(pairCommand).")
        }
        self.configuration = configuration
        self.ticket = ticket
        self.peer = peer
        // A link need not carry a name; show its host then, never an endpoint.
        let named = (value["name"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let host = URLComponents(string: configuration["endpoint"] as? String ?? "")?.host
        self.name = named.isEmpty ? host ?? peer : named
    }

    /// The computer's host name alone: what a person can check, without a
    /// scheme, port or relay route.
    var host: String {
        URLComponents(string: configuration["endpoint"] as? String ?? "")?.host ?? peer
    }
}

/// Why enrollment failed, in the few classes a person can act on. The raw
/// error never reaches the screen or support evidence: transport and
/// Keychain text can carry an endpoint or a credential.
enum PairingFailure: Error, Equatable {
    case unreachable, codeRejected, deviceKey, other

    init(_ error: Error) {
        let text = error.localizedDescription.lowercased()
        func mentions(_ phrases: String...) -> Bool { phrases.contains { text.contains($0) } }
        if mentions("keychain", "secure enclave", "device key", "enrollment key") { self = .deviceKey }
        else if mentions("refused the pairing token", "enrollment refused", "enrollment ticket") { self = .codeRejected }
        else if mentions("did not answer", "stopped answering", "could not resolve", "resolved to no addresses") {
            self = .unreachable
        } else { self = .other }
    }

    /// Nothing was spent, so the same code can simply be tried again.
    var keepsCode: Bool { self == .unreachable || self == .deviceKey }

    func problem(name: String) -> PairingFlow.Problem {
        switch self {
        case .unreachable:
            .init(title: "Couldn't reach \(name)",
                  message: "Check that the computer is awake and running Phux, and that this phone is on the same network or Tailscale. Then try again.")
        case .codeRejected:
            .init(title: "This pairing code can't be used",
                  message: "It has expired or was already used. On the computer, run \(pairCommand) for a fresh code.")
        case .deviceKey:
            .init(title: "This phone couldn't create its key",
                  message: "Unlock the phone and try again.")
        case .other:
            .init(title: "Couldn't pair with this computer",
                  message: "On the computer, run \(pairCommand) for a fresh code, then try again.")
        }
    }
}

/// One import, one explicit confirmation, one enrollment. A failed Keychain
/// save retries the issued identity, never the consumed enrollment ticket.
@MainActor
final class PairingFlow {
    typealias Enrollment = @MainActor (PairingInvite) async throws -> [String: Any]
    /// Saves an issued identity under the computer's display name.
    typealias Save = @MainActor ([String: Any], String) throws -> Void
    typealias Store = @MainActor ([String: Any]) throws -> Void
    enum Stage {
        case welcome
        case review(PairingInvite)
        case enrolling(String)
        case finish([String: Any], name: String)
    }
    struct Problem: Equatable {
        let title: String
        let message: String
    }

    private(set) var stage: Stage = .welcome
    private(set) var problem: Problem?
    var message: String? { problem?.message }
    var changed: (() -> Void)?
    var connected: (([String: Any]) -> Void)?
    /// Every new problem, after `changed`: the moment to interrupt.
    var failed: ((Problem) -> Void)?
    private let enroll: Enrollment
    private let save: Save
    private let discard: Store
    private var operation: Task<Void, Never>?
    private var generation = 0

    init(enroll: @escaping Enrollment = {
        try await NativeIdentity.enroll(configuration: $0.configuration, ticket: $0.ticket)
    }, save: @escaping Save = { try HostSettings.save($0, name: $1) },
         discard: @escaping Store = NativeIdentity.discard) {
        self.enroll = enroll; self.save = save; self.discard = discard
    }

    isolated deinit {
        operation?.cancel()
        if case .finish(let issued, _) = stage { try? discard(issued) }
    }

    @discardableResult
    func receive(_ link: String) -> Bool {
        guard operation == nil else {
            report(Problem(title: "Pairing is already under way",
                           message: "Wait for it to finish, or cancel it, before opening another code."))
            return false
        }
        guard let invite = try? PairingInvite(link: link) else {
            // The parser's reasons name pins and tickets; a person needs one instruction.
            report(Problem(title: "That isn't a pairing link",
                           message: "Use the whole link or QR code that \(pairCommand) shows on your computer."))
            return false
        }
        cancel()
        stage = .review(invite)
        OnboardingStatus.record(.awaitingConfirmation, invite: invite)
        changed?()
        return true
    }

    private func report(_ problem: Problem) {
        self.problem = problem
        changed?()
        failed?(problem)
    }

    func confirm() {
        switch stage {
        case .review(let invite): begin(invite)
        case .finish(let issued, let name): saveIssued(issued, name: name)
        default: break
        }
    }

    func cancel() {
        generation += 1
        operation?.cancel(); operation = nil
        if case .finish(let issued, _) = stage { try? discard(issued) }
        stage = .welcome
        problem = nil
        changed?()
    }

    private func begin(_ invite: PairingInvite) {
        guard operation == nil else { return }
        generation += 1
        let current = generation
        stage = .enrolling(invite.name)
        problem = nil
        OnboardingStatus.record(.enrolling, invite: invite)
        changed?()
        let enroll = enroll, discard = discard
        operation = Task { [weak self] in
            do {
                let issued = try await enroll(invite)
                guard let self, self.generation == current, !Task.isCancelled else {
                    try discard(issued)
                    return
                }
                self.operation = nil
                OnboardingStatus.record(.identityIssued, invite: invite)
                self.saveIssued(issued, name: invite.name)
            } catch {
                guard let self, self.generation == current else { return }
                self.operation = nil
                let failure = PairingFailure(error)
                self.stage = failure.keepsCode ? .review(invite) : .welcome
                OnboardingStatus.record(.enrollmentFailed, invite: invite, failure: failure)
                self.report(failure.problem(name: invite.name))
            }
        }
    }

    private func saveIssued(_ issued: [String: Any], name: String) {
        do {
            try save(issued, name)
            stage = .welcome
            problem = nil
            changed?()
            connected?(issued)
        } catch {
            stage = .finish(issued, name: name)
            report(Problem(title: "Couldn't save this pairing",
                           message: "This phone is paired, but couldn't save its key. Unlock the phone and try again; you don't need another code."))
        }
    }

    #if targetEnvironment(simulator)
    var qualificationOperation: Task<Void, Never>? { operation }
    #endif
}
