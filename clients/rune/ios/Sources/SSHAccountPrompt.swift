import UIKit
import RuneTailnet

/// A tailnet owner is not an operating-system account. Remember only an
/// account that actually completed SSH; never guess it from an email address.
@MainActor
enum SSHAccountPrompt {
    private static let preference = "tailnet-ssh-accounts"

    static func saved(for peer: TailnetPeer) -> String? {
        UserDefaults.standard.dictionary(forKey: preference)?[peer.id] as? String
    }

    static func remember(_ username: String, for peer: TailnetPeer) {
        var accounts = UserDefaults.standard.dictionary(forKey: preference) ?? [:]
        accounts[peer.id] = username
        UserDefaults.standard.set(accounts, forKey: preference)
    }

    static func make(peer: TailnetPeer, username: String?,
                     confirm: @escaping (String, String?) -> Void) -> UIAlertController {
        let guidance = peer.servesTailscaleSSH
            ? "Enter the account name on this computer. Tailscale SSH handles sign-in; your tailnet may ask for browser approval."
            : "Use this computer's login account and password—not your Tailscale email, Apple ID, or phone passcode. On a Mac, the account name is the short name shown by whoami in Terminal."
        let alert = UIAlertController(title: "Sign in to \(peer.name)",
            message: guidance + "\n\nSSH only gets a one-time Phux pairing code. Nothing is installed or upgraded.", preferredStyle: .alert)
        alert.addTextField { field in
            field.placeholder = "Computer account name"
            field.text = username
            field.textContentType = .username
            field.autocapitalizationType = .none
            field.autocorrectionType = .no
            field.accessibilityIdentifier = "ssh-computer-account"
        }
        if !peer.servesTailscaleSSH {
            alert.addTextField { field in
                field.placeholder = "Computer login password"
                field.isSecureTextEntry = true
                field.textContentType = .password
                field.accessibilityIdentifier = "ssh-computer-password"
            }
        }
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        alert.addAction(UIAlertAction(title: "Get pairing code", style: .default) { [weak alert] _ in
            let user = (alert?.textFields?.first?.text ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let password = alert?.textFields?.dropFirst().first?.text
            alert?.textFields?.forEach { $0.text = nil }
            confirm(user, password)
        })
        return alert
    }

    static func failure(peer: TailnetPeer, message: String, retry: @escaping () -> Void,
                        back: @escaping () -> Void) -> UIAlertController {
        let alert = UIAlertController(title: "Could not connect to \(peer.name)", message: message, preferredStyle: .alert)
        alert.addAction(UIAlertAction(title: "Try again", style: .default) { _ in retry() })
        alert.addAction(UIAlertAction(title: "Back to QR / paste", style: .cancel) { _ in back() })
        return alert
    }
}
