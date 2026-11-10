import UIKit
import RuneTailnet

/// A tailnet owner is not an operating-system account. Remember only an
/// account that actually completed SSH; never guess it from an email address.
/// The password lives in the alert's field until it is handed over, and is
/// never stored or logged.
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
            ? "Enter your account name on this computer. Tailscale handles the sign-in."
            : "Use this computer's own login name and password, not your Tailscale or Apple ID sign-in. Rune uses them once to get a pairing code."
        let alert = UIAlertController.dark(title: "Sign in to \(peer.name)", message: guidance)
        let submit = UIAlertAction(title: "Get Pairing Code", style: .default) { [weak alert] _ in
            let user = name(alert?.textFields?.first)
            let password = alert?.textFields?.dropFirst().first?.text
            alert?.textFields?.forEach { $0.text = nil }
            confirm(user, password)
        }
        alert.addTextField { [weak submit] field in
            field.placeholder = "Account name (whoami)"
            field.text = username
            field.textContentType = .username
            field.autocapitalizationType = .none
            field.autocorrectionType = .no
            field.accessibilityIdentifier = "ssh-computer-account"
            // A blank name can only fail; do not offer to send it.
            field.addAction(UIAction { [weak submit, weak field] _ in
                submit?.isEnabled = !name(field).isEmpty
            }, for: .editingChanged)
            submit?.isEnabled = !name(field).isEmpty
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
        alert.addAction(submit)
        alert.preferredAction = submit
        return alert
    }

    private static func name(_ field: UITextField?) -> String {
        (field?.text ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    static func failure(peer: TailnetPeer, message: String, retry: @escaping () -> Void,
                        back: @escaping () -> Void) -> UIAlertController {
        let alert = UIAlertController.dark(title: "Couldn't get a code from \(peer.name)", message: message)
        let again = UIAlertAction(title: "Try Again", style: .default) { _ in retry() }
        alert.addAction(again)
        alert.addAction(UIAlertAction(title: "Scan or Paste Instead", style: .default) { _ in back() })
        // Staying put is a choice too: the list still explains what failed.
        alert.addAction(UIAlertAction(title: "OK", style: .cancel))
        alert.preferredAction = again
        return alert
    }
}
