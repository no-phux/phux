import UIKit
import Security

@MainActor
enum HostSettings {
    static let service = "sh.phux.rune.ios"
    private static let nameKey = "paired-computer-name"

    /// What the person calls the saved computer. Kept beside the saved
    /// configuration, never inside it: the shared parser rejects unknown fields.
    static var computerName: String? {
        guard let endpoint = UserDefaults.standard.dictionary(forKey: "remote-host")?["endpoint"] as? String else { return nil }
        return UserDefaults.standard.string(forKey: nameKey) ?? URLComponents(string: endpoint)?.host
    }

    static func load() throws -> [String: Any]? {
        guard var values = UserDefaults.standard.dictionary(forKey: "remote-host"),
              let endpoint = values["endpoint"] as? String else { return nil }
        let query: [String: Any] = [kSecClass as String:kSecClassGenericPassword,
            kSecAttrService as String:service, kSecAttrAccount as String:endpoint,
            kSecReturnData as String:true, kSecMatchLimit as String:kSecMatchLimitOne]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw NativeFailure.message("Could not read the pairing credential (Keychain \(status)).")
        }
        if status == errSecSuccess {
            guard let data = result as? Data, let token = String(data: data, encoding: .utf8) else {
                throw NativeFailure.message("The saved pairing credential is not valid UTF-8.")
            }
            values["token"] = token
        }
        return values
    }
    static func save(_ values: [String: Any], name: String? = nil) throws {
        try NativeBridge.validate(values)
        if values["chain_pem"] != nil {
            try NativeIdentity(configuration: values).validate(configuration: values)
        }
        guard let endpoint = values["endpoint"] as? String else {
            throw NativeFailure.message("The computer's address is missing.")
        }
        let previous = UserDefaults.standard.dictionary(forKey:"remote-host")?["endpoint"] as? String
        let query: [String: Any] = [kSecClass as String:kSecClassGenericPassword,
            kSecAttrService as String:service, kSecAttrAccount as String:endpoint]
        let data = Data((values["token"] as? String ?? "").utf8)
        let status = SecItemUpdate(query as CFDictionary,[kSecValueData as String:data,
            kSecAttrAccessible as String:kSecAttrAccessibleWhenUnlockedThisDeviceOnly] as CFDictionary)
        if status == errSecItemNotFound {
            var add = query
            add[kSecValueData as String] = data
            add[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            let added = SecItemAdd(add as CFDictionary,nil)
            guard added == errSecSuccess else { throw NativeFailure.message("Could not save the pairing credential (Keychain \(added)).") }
        } else if status != errSecSuccess {
            throw NativeFailure.message("Could not update the pairing credential (Keychain \(status)).")
        }
        let publicKeys = ["endpoint", "certificate_pin", "authority_ca", "chain_pem", "key_id", "session", "tls_server_name"]
        let publicValues = values.filter { publicKeys.contains($0.key) }
        UserDefaults.standard.set(publicValues,forKey:"remote-host")
        // A name describes one computer; never let it outlive a change of host.
        if let name, !name.isEmpty { UserDefaults.standard.set(String(name.prefix(128)), forKey: nameKey) }
        else if previous != endpoint { UserDefaults.standard.removeObject(forKey: nameKey) }
        // Commit the new working credential before retiring the previous one.
        if let previous, previous != endpoint {
            do { try removeToken(previous) }
            catch { NSLog("Previous token cleanup deferred: %@",error.localizedDescription) }
        }
    }

    private static func removeToken(_ endpoint:String) throws {
        let query:[String:Any] = [kSecClass as String:kSecClassGenericPassword,
            kSecAttrService as String:service,kSecAttrAccount as String:endpoint]
        let status = SecItemDelete(query as CFDictionary)
        let removed = status == errSecSuccess || status == errSecItemNotFound
        var pending = Set(UserDefaults.standard.stringArray(forKey:"pending-token-cleanup") ?? [])
        if removed { pending.remove(endpoint) } else { pending.insert(endpoint) }
        UserDefaults.standard.set(Array(pending),forKey:"pending-token-cleanup")
        guard removed else {
            throw NativeFailure.message("New computer saved, but could not remove the previous pairing token (Keychain \(status)); cleanup will retry when the app becomes active.")
        }
    }

    static func retryPendingCleanup() -> [String] {
        (UserDefaults.standard.stringArray(forKey:"pending-token-cleanup") ?? []).compactMap { endpoint in
            do { try removeToken(endpoint); return nil }
            catch { return error.localizedDescription }
        }
    }
}

@MainActor
final class ConnectionForm: UIViewController, UITextFieldDelegate {
    var connected: (([String: Any]) -> Void)?
    private let initial: [String: Any]
    private let endpoint = UITextField()
    private let pin = UITextField()
    private let token = UITextField()
    private let authority = UITextField()
    private let ticket = UITextField()
    private let session = UITextField()
    private let route = UITextField()
    private let status = UILabel()
    private let scroll = UIScrollView()
    private let enroll = UIButton(configuration: .borderedProminent())
    private let connect = UIButton(configuration: .bordered())
    private var enrolled: [String: Any]
    private var enrollment: Task<Void, Never>?
    private var generation = 0
    isolated deinit { enrollment?.cancel() }

    private var fields: [UITextField] { [endpoint, pin, authority, token, ticket, session, route] }

    init(values: [String: Any]?) {
        initial = values ?? [:]; enrolled = values ?? [:]
        super.init(nibName:nil,bundle:nil)
    }
    @available(*, unavailable) required init?(coder:NSCoder) { fatalError() }
    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Advanced setup"
        overrideUserInterfaceStyle = .dark
        view.backgroundColor = .systemBackground
        let body = UIStackView(); body.axis = .vertical; body.spacing = 18
        body.translatesAutoresizingMaskIntoConstraints = false
        let explanation = UILabel()
        explanation.text = "For troubleshooting or a custom address. Copy each value from \(pairCommand) on your computer. To pair the usual way, go back and scan its QR code."
        explanation.numberOfLines = 0; explanation.textColor = .secondaryLabel
        explanation.font = .preferredFont(forTextStyle:.body); explanation.adjustsFontForContentSizeCategory = true
        body.addArrangedSubview(explanation)
        for (field,key,title,hint) in [(endpoint,"endpoint","Address","quic://host:8788 or wss://host"),
                                        (pin,"certificate_pin","Certificate pin","SHA-256 fingerprint"),
                                        (authority,"authority_ca","Authority pin","sha256:…"),
                                        (token,"token","Pairing token","64 hex digits"),
                                        (ticket,"ticket","One-time enrollment ticket","Needed to enroll, never saved"),
                                        (session,"session","Session","Optional"),
                                        (route,"tls_server_name","Relay route","Optional")] {
            field.text = initial[key] as? String
            body.addArrangedSubview(labelled(field, title: title, hint: hint))
        }
        endpoint.keyboardType = .URL; endpoint.textContentType = .URL
        token.isSecureTextEntry = true; ticket.isSecureTextEntry = true
        route.returnKeyType = .done
        enroll.configuration?.title = "Enroll & Connect"
        enroll.addTarget(self,action:#selector(enrollPhone),for:.touchUpInside)
        connect.configuration?.title = "Connect"
        connect.addTarget(self,action:#selector(connectHost),for:.touchUpInside)
        for button in [enroll, connect] {
            button.configuration?.buttonSize = .large
            button.heightAnchor.constraint(greaterThanOrEqualToConstant:44).isActive = true
            body.addArrangedSubview(button)
        }
        status.numberOfLines = 0; status.textColor = .systemRed
        status.font = .preferredFont(forTextStyle:.footnote); status.adjustsFontForContentSizeCategory = true
        body.addArrangedSubview(status)
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.keyboardDismissMode = .interactive
        view.addSubview(scroll); scroll.addSubview(body)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo:view.safeAreaLayoutGuide.topAnchor),
            scroll.leadingAnchor.constraint(equalTo:view.leadingAnchor),scroll.trailingAnchor.constraint(equalTo:view.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo:view.keyboardLayoutGuide.topAnchor),
            body.topAnchor.constraint(equalTo:scroll.contentLayoutGuide.topAnchor,constant:20),
            body.bottomAnchor.constraint(equalTo:scroll.contentLayoutGuide.bottomAnchor,constant:-20),
            body.leadingAnchor.constraint(equalTo:scroll.contentLayoutGuide.leadingAnchor,constant:20),
            body.trailingAnchor.constraint(equalTo:scroll.contentLayoutGuide.trailingAnchor,constant:-20),
            body.widthAnchor.constraint(equalTo:scroll.frameLayoutGuide.widthAnchor,constant:-40)])
    }

    /// A placeholder vanishes with the first character; the title stays.
    private func labelled(_ field: UITextField, title: String, hint: String) -> UIView {
        let label = UILabel()
        label.text = title; label.textColor = .secondaryLabel; label.numberOfLines = 0
        label.font = .preferredFont(forTextStyle:.subheadline); label.adjustsFontForContentSizeCategory = true
        label.isAccessibilityElement = false // The field carries the same name.
        field.placeholder = hint; field.accessibilityLabel = title
        field.font = .preferredFont(forTextStyle:.body); field.adjustsFontForContentSizeCategory = true
        field.borderStyle = .roundedRect
        field.keyboardType = .asciiCapable; field.returnKeyType = .next
        field.autocapitalizationType = .none; field.autocorrectionType = .no
        field.spellCheckingType = .no
        field.smartQuotesType = .no; field.smartDashesType = .no
        field.clearButtonMode = .whileEditing
        field.delegate = self
        field.heightAnchor.constraint(greaterThanOrEqualToConstant:44).isActive = true
        let group = UIStackView(arrangedSubviews:[label, field]); group.axis = .vertical; group.spacing = 6
        return group
    }

    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        let next = fields.firstIndex(of: textField).map { $0 + 1 } ?? fields.count
        if next < fields.count { fields[next].becomeFirstResponder() } else { textField.resignFirstResponder() }
        return false
    }

    /// An outcome below the keyboard or off the screen was never delivered.
    private func report(_ text: String?, failed: Bool = true) {
        status.textColor = failed ? .systemRed : .secondaryLabel
        status.text = text
        guard let text else { return }
        view.layoutIfNeeded()
        scroll.scrollRectToVisible(status.convert(status.bounds, to: scroll), animated: true)
        UIAccessibility.post(notification: .announcement, argument: text)
    }

    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        generation += 1
        enrollment?.cancel()
        enrollment = nil
        setEnrolling(false)
        ticket.text = nil
    }

    private func configuration() -> [String: Any] {
        var values: [String: Any] = [:]
        for (key, field) in [("endpoint", endpoint), ("certificate_pin", pin),
                             ("authority_ca", authority), ("token", token), ("session", session),
                             ("tls_server_name", route)] {
            let text = (field.text ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            if !text.isEmpty { values[key] = text }
        }
        // Editing host ownership/trust invalidates the saved chain. Session
        // changes alone do not. Never spend a ticket using an old CA silently.
        let ownership = ["certificate_pin", "authority_ca", "token", "tls_server_name"]
        let newHost = URLComponents(string: values["endpoint"] as? String ?? "")?.host?.lowercased()
        let savedHost = URLComponents(string: enrolled["endpoint"] as? String ?? "")?.host?.lowercased()
        let sameHost = newHost != nil && newHost == savedHost
        if sameHost && ownership.allSatisfy({ values[$0] as? String == enrolled[$0] as? String }) {
            values["chain_pem"] = enrolled["chain_pem"]
            values["key_id"] = enrolled["key_id"]
        }
        return values
    }

    private func setEnrolling(_ busy: Bool) {
        for field in fields { field.isEnabled = !busy }
        enroll.isEnabled = !busy; connect.isEnabled = !busy
        isModalInPresentation = busy
        enroll.configuration?.title = busy ? "Enrolling…" : "Enroll & Connect"
        enroll.configuration?.showsActivityIndicator = busy
    }

    @objc private func enrollPhone() {
        guard enrollment == nil else { return }
        let values = configuration()
        let oneTimeTicket = ticket.text ?? ""
        ticket.text = nil
        view.endEditing(true)
        report(nil)
        generation += 1
        let operation = generation
        setEnrolling(true)
        // Do not retain the form across the network operation. A dismissed
        // form cannot install a late chain, even if the ticket was accepted.
        enrollment = Task { [weak self] in
            var issued: [String: Any]?
            do {
                let identity = try await NativeIdentity.enroll(configuration: values, ticket: oneTimeTicket)
                guard let self, self.generation == operation, !Task.isCancelled else {
                    try NativeIdentity.discard(configuration: identity)
                    return
                }
                issued = identity
            } catch {
                guard let self, self.generation == operation else {
                    // Keystore cleanup failures remain visible even after the
                    // form disappears. No credential or ticket is logged.
                    NSLog("Device enrollment did not complete: %@", error.localizedDescription)
                    return
                }
                self.report(error.localizedDescription)
            }
            guard let self, self.generation == operation else { return }
            self.enrollment = nil
            self.setEnrolling(false)
            if let issued { self.finishEnrollment(issued) }
        }
    }

    private func finishEnrollment(_ issued: [String: Any]) {
        // Keep the newly issued identity in memory if saving fails: the
        // one-time ticket is already spent, so Connect can retry the save.
        enrolled = issued
        do {
            // Saved and opened together: an enrolled form that could still be
            // backed out of left the saved computer and the live one apart.
            try HostSettings.save(issued)
            connected?(issued)
        } catch {
            report("This phone is enrolled, but saving failed: \(error.localizedDescription) Tap Connect to try again; you don't need another ticket.")
        }
    }

    @objc private func connectHost() {
        guard enrollment == nil else { return }
        let values = configuration()
        do { try HostSettings.save(values); view.endEditing(true); connected?(values) }
        catch { report(error.localizedDescription) }
    }
}
