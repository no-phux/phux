import Foundation
import RuneTailnet

/// Local, bounded support evidence. No password, token, ticket, banner or raw
/// error is accepted by this API. It never uploads or changes authentication.
@MainActor
enum OnboardingStatus {
    enum Phase: String {
        case connecting, codeReceived = "code_received", failed
        case importFailed = "import_failed"
        case awaitingConfirmation = "awaiting_confirmation"
        case enrolling, identityIssued = "identity_issued", enrollmentFailed = "enrollment_failed"
    }

    static func record(_ phase: Phase, peer: TailnetPeer, failure: Error? = nil) {
        write(phase, peer: peer.dnsName, failure: failure)
    }

    static func record(_ phase: Phase, invite: PairingInvite, failure: Error? = nil) {
        write(phase, peer: invite.peer, failure: failure)
    }

    private static func write(_ phase: Phase, peer: String, failure: Error?) {
        let report: [String: Any] = ["schema": 1, "phase": phase.rawValue, "peer": String(peer.prefix(512)),
            "time": Date().timeIntervalSince1970, "failure": failure.map(code) ?? "none"]
        do {
            let directory = try FileManager.default.url(for: .documentDirectory, in: .userDomainMask,
                                                         appropriateFor: nil, create: true)
            var path = directory.appendingPathComponent("rune-onboarding-status.json")
            try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]).write(
                to: path, options: [.atomic, .completeFileProtection])
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try path.setResourceValues(values)
        } catch { /* Support evidence must not block or alter pairing. */ }
    }

    static func code(_ error: Error) -> String {
        if let error = error as? SSHFailure { return sshCode(error) }
        if let error = error as? TailnetPairingError { return pairingCode(error) }
        if let error = error as? PairingFailure { return enrollmentCode(error) }
        return "unknown"
    }

    private static func enrollmentCode(_ error: PairingFailure) -> String {
        switch error {
        case .unreachable: "enroll_unreachable"
        case .codeRejected: "enroll_code_rejected"
        case .deviceKey: "enroll_device_key"
        case .other: "unknown"
        }
    }

    private static func sshCode(_ error: SSHFailure) -> String {
        switch error {
        case .authenticationFailed: "ssh_authentication"
        case .hostKeyMismatch: "ssh_host_key"
        case .unreachable: "ssh_unreachable"
        case .timedOut: "ssh_timeout"
        case .commandRefused: "ssh_command"
        case .outputTooLarge: "ssh_output_limit"
        }
    }

    private static func pairingCode(_ error: TailnetPairingError) -> String {
        switch error {
        case .systemTunnelRequired: "vpn_required"
        case .otherTailnet: "vpn_tailnet_mismatch"
        case .invalidPeer: "invalid_peer"
        case .invalidUsername: "invalid_account"
        case .phuxNotInstalled: "phux_not_installed"
        case .pairFailed: "server_pairing"
        case .quicRequired: "quic_required"
        case .invalidPairing: "pairing_document"
        case .destinationMismatch: "pairing_destination"
        }
    }
}
