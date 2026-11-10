import Foundation

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
            throw NativeFailure.message("This pairing link is incomplete. Make a fresh code with phux pair --enroll.")
        }
        self.configuration = configuration
        self.ticket = ticket
        self.peer = peer
        self.name = value["name"] as? String ?? peer
    }
}

/// One import, one explicit confirmation, one enrollment. A failed Keychain
/// save retries the issued identity, never the consumed enrollment ticket.
@MainActor
final class PairingFlow {
    typealias Enrollment = @MainActor (PairingInvite) async throws -> [String: Any]
    typealias Store = @MainActor ([String: Any]) throws -> Void
    enum Stage {
        case welcome
        case review(PairingInvite)
        case enrolling(String)
        case finish([String: Any])
    }

    private(set) var stage: Stage = .welcome
    private(set) var message: String?
    var changed: (() -> Void)?
    var connected: (([String: Any]) -> Void)?
    private let enroll: Enrollment
    private let save: Store
    private let discard: Store
    private var operation: Task<Void, Never>?
    private var generation = 0

    init(enroll: @escaping Enrollment = {
        try await NativeIdentity.enroll(configuration: $0.configuration, ticket: $0.ticket)
    }, save: @escaping Store = HostSettings.save,
         discard: @escaping Store = NativeIdentity.discard) {
        self.enroll = enroll; self.save = save; self.discard = discard
    }

    isolated deinit {
        operation?.cancel()
        if case .finish(let issued) = stage { try? discard(issued) }
    }

    @discardableResult
    func receive(_ link: String) -> Bool {
        guard operation == nil else {
            message = "Finish or cancel the current pairing before opening another code."
            changed?()
            return false
        }
        do {
            let invite = try PairingInvite(link: link)
            cancel()
            stage = .review(invite)
            message = nil
            OnboardingStatus.record(.awaitingConfirmation, invite: invite)
            changed?()
            return true
        } catch {
            message = error.localizedDescription
            changed?()
            return false
        }
    }

    func confirm() {
        switch stage {
        case .review(let invite): begin(invite)
        case .finish(let issued): saveIssued(issued)
        default: break
        }
    }

    func cancel() {
        generation += 1
        operation?.cancel(); operation = nil
        if case .finish(let issued) = stage { try? discard(issued) }
        stage = .welcome
        message = nil
        changed?()
    }

    private func begin(_ invite: PairingInvite) {
        guard operation == nil else { return }
        generation += 1
        let current = generation
        stage = .enrolling(invite.name)
        message = nil
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
                self.saveIssued(issued)
            } catch {
                guard let self, self.generation == current else { return }
                self.operation = nil
                self.stage = .welcome
                OnboardingStatus.record(.enrollmentFailed, invite: invite, failure: error)
                self.message = "Couldn't pair this phone. Check Tailscale, make a fresh pairing code on the computer, and try again."
                self.changed?()
            }
        }
    }

    private func saveIssued(_ issued: [String: Any]) {
        do {
            try save(issued)
            stage = .welcome
            message = nil
            changed?()
            connected?(issued)
        } catch {
            stage = .finish(issued)
            message = "This phone is paired, but its credential couldn't be saved. Unlock the phone and retry; you don't need another code."
            changed?()
        }
    }

    #if targetEnvironment(simulator)
    var qualificationOperation: Task<Void, Never>? { operation }
    #endif
}
