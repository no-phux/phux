import UIKit
import Security

@MainActor
enum HostSettings {
    static let service = "sh.phux.rune.ios"
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
    static func save(_ values: [String: Any]) throws {
        try NativeBridge.validate(values)
        if values["chain_pem"] != nil {
            try NativeIdentity(configuration: values).validate(configuration: values)
        }
        guard let endpoint = values["endpoint"] as? String else {
            throw NativeFailure.message("The host endpoint is missing.")
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
            throw NativeFailure.message("New host saved, but could not remove the previous pairing token (Keychain \(status)); cleanup will retry when the app becomes active.")
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
final class ConnectionForm: UIViewController {
    var connected: (([String: Any]) -> Void)?
    private let initial: [String: Any]
    private let endpoint = UITextField()
    private let pin = UITextField()
    private let token = UITextField()
    private let authority = UITextField()
    private let ticket = UITextField()
    private let session = UITextField()
    private let route = UITextField()
    private let error = UILabel()
    private let enroll = UIButton(type: .system)
    private let connect = UIButton(type: .system)
    private var enrolled: [String: Any]
    private var enrollment: Task<Void, Never>?
    private var generation = 0
    isolated deinit { enrollment?.cancel() }

    init(values: [String: Any]?) {
        initial = values ?? [:]; enrolled = values ?? [:]
        super.init(nibName:nil,bundle:nil)
    }
    @available(*, unavailable) required init?(coder:NSCoder) { fatalError() }
    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Advanced setup"
        view.backgroundColor = .systemBackground
        let body = UIStackView(); body.axis = .vertical; body.spacing = 14
        body.translatesAutoresizingMaskIntoConstraints = false
        let explanation = UILabel()
        explanation.text = "Use the endpoint, certificate pin, authority pin, token and one-time ticket from phux pair --enroll. Enroll over QUIC first; this phone keeps its private key in the platform keystore. Never paste a private key."
        explanation.numberOfLines = 0; explanation.font = .preferredFont(forTextStyle:.body)
        body.addArrangedSubview(explanation)
        for (field,key,label) in [(endpoint,"endpoint","quic://host:8788 or wss://host"),
                                   (pin,"certificate_pin","SHA-256 certificate pin"),
                                   (authority,"authority_ca","Authority pin (sha256:…)"),
                                   (token,"token","Pairing token"),
                                   (ticket,"ticket","One-time enrollment ticket"),
                                   (session,"session","Session (optional)"),
                                   (route,"tls_server_name","Relay route (optional)")] {
            field.text = initial[key] as? String
            field.placeholder = label; field.accessibilityLabel = label
            field.borderStyle = .roundedRect
            field.autocapitalizationType = .none; field.autocorrectionType = .no
            field.smartQuotesType = .no; field.smartDashesType = .no
            field.heightAnchor.constraint(greaterThanOrEqualToConstant:44).isActive = true
            body.addArrangedSubview(field)
        }
        token.isSecureTextEntry = true; ticket.isSecureTextEntry = true
        enroll.setTitle("Enroll this phone",for:.normal)
        enroll.addTarget(self,action:#selector(enrollPhone),for:.touchUpInside)
        enroll.heightAnchor.constraint(greaterThanOrEqualToConstant:44).isActive = true
        body.addArrangedSubview(enroll)
        connect.setTitle("Connect",for:.normal); connect.addTarget(self,action:#selector(connectHost),for:.touchUpInside)
        connect.heightAnchor.constraint(greaterThanOrEqualToConstant:44).isActive = true
        body.addArrangedSubview(connect)
        error.numberOfLines = 0; error.textColor = .systemRed; body.addArrangedSubview(error)
        let scroll = UIScrollView(); scroll.translatesAutoresizingMaskIntoConstraints = false
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
        for field in [endpoint, pin, authority, token, ticket, session, route] { field.isEnabled = !busy }
        enroll.isEnabled = !busy; connect.isEnabled = !busy
        isModalInPresentation = busy
        enroll.setTitle(busy ? "Enrolling…" : "Enroll this phone", for: .normal)
    }

    @objc private func enrollPhone() {
        guard enrollment == nil else { return }
        let values = configuration()
        let oneTimeTicket = ticket.text ?? ""
        ticket.text = nil
        error.textColor = .systemRed
        error.text = nil
        generation += 1
        let operation = generation
        setEnrolling(true)
        // Do not retain the form across the network operation. A dismissed
        // form cannot install a late chain, even if the ticket was accepted.
        enrollment = Task { [weak self] in
            do {
                let issued = try await NativeIdentity.enroll(configuration: values, ticket: oneTimeTicket)
                guard let self, self.generation == operation, !Task.isCancelled else {
                    try NativeIdentity.discard(configuration: issued)
                    return
                }
                self.finishEnrollment(issued)
            } catch {
                guard let self, self.generation == operation else {
                    // Keystore cleanup failures remain visible even after the
                    // form disappears. No credential or ticket is logged.
                    NSLog("Device enrollment did not complete: %@", error.localizedDescription)
                    return
                }
                self.error.text = error.localizedDescription
            }
            guard let self, self.generation == operation else { return }
            self.enrollment = nil
            self.setEnrolling(false)
        }
    }

    private func finishEnrollment(_ issued: [String: Any]) {
        // Keep the newly issued identity in memory if saving fails: the
        // one-time ticket is already spent, so Connect can retry the save.
        enrolled = issued
        do {
            try HostSettings.save(issued)
            error.textColor = .secondaryLabel
            error.text = "This phone is enrolled. Connect to open the host."
        } catch {
            self.error.textColor = .systemRed
            self.error.text = error.localizedDescription
        }
    }

    @objc private func connectHost() {
        guard enrollment == nil else { return }
        let values = configuration()
        error.textColor = .systemRed
        do { try HostSettings.save(values); view.endEditing(true); connected?(values) }
        catch { self.error.text = error.localizedDescription }
    }
}
