import Foundation
import Security

/// The computers paired with the earlier (Swift) app under this bundle
/// identifier. Every one that can be carried whole comes over, the last used
/// open. Everything the earlier app stored is only read: its records, tokens
/// and keys stay, and each carried computer keeps its own key in place.
@MainActor
enum EarlierHosts {
    /// `ServerConfig` as the earlier app kept it in UserDefaults.
    struct Host: Decodable {
        struct Mode: Decodable {
            struct Live: Decodable { let url: String }
            let live: Live?
        }
        let id: String
        let name: String
        let mode: Mode
        let fingerprint: String?
        let authority: String?
        let relayRoute: String?
        let token: String?
        var title: String { name.isEmpty ? URLComponents(string: mode.live?.url ?? "")?.host ?? "A computer" : name }
    }
    private struct Envelope<Value: Decodable>: Decodable { let v: Int; let data: Value }
    private struct Identity: Decodable { let chain: String; let keyID: String }

    /// Why one computer was not carried, in words to show.
    private struct Refusal: LocalizedError {
        let errorDescription: String?
        init(_ reason: String) { errorDescription = reason }
    }
    /// The Keychain did not answer, so nothing is known about any computer.
    /// Nothing is carried: the next launch reads it again.
    struct Unreadable: Error { let status: OSStatus }

    @MainActor
    struct Outcome {
        /// The computer left open: the last used of those carried.
        var carried: String?
        /// What was not carried, each with its reason when one is known.
        var left: [(name: String, reason: String?)] = []
        var advice = "On your computer, run \(pairCommand), then pair again."

        /// Shown once beside the carried computer.
        var notice: String? {
            guard let carried, !left.isEmpty else { return nil }
            return "\(carried) was carried over, but \(EarlierHosts.list(left)) \(left.count == 1 ? "was" : "were") not.\(reasons) "
                + "To use \(left.count == 1 ? "it" : "them"), run \(pairCommand) there and pair it here."
        }
        /// Shown in place of a terminal when nothing could be carried.
        var refusal: String {
            "The update couldn't carry over \(left.isEmpty ? "the computers you had paired" : EarlierHosts.list(left)).\(reasons) \(advice)"
        }
        private var reasons: String {
            left.compactMap { host in host.reason.map { " \(host.name): \($0)." } }.joined()
        }
    }

    /// Carry every computer that can be carried whole, at most once: a phone
    /// Rune already paired is left alone. The last used is saved last, so it
    /// is the one open. Nil when the earlier app left no computers.
    static func carry(defaults: UserDefaults = .standard, paired: Bool = HostSettings.computerName != nil,
                      credential: @MainActor (String) throws -> Data? = { try keychain($0) },
                      save: ([String: Any], String) throws -> Void = { try HostSettings.save($0, name: $1) }) -> Outcome? {
        guard !paired, defaults.data(forKey: "phux.savedServers") != nil else { return nil }
        guard let hosts = roster(defaults) else { return Outcome() }
        guard !hosts.isEmpty else { return nil }
        var outcome = Outcome()
        do {
            for host in hosts.reversed() {
                do {
                    try save(configuration(host, credential: credential), host.title)
                    outcome.carried = host.title
                } catch let error as Unreadable { throw error }
                catch let error as Refusal { outcome.left.append((host.title, error.localizedDescription)) }
                catch {
                    NSLog("An earlier pairing was not carried: %@", error.localizedDescription)
                    outcome.left.append((host.title, "its pairing could not be verified"))
                }
            }
        } catch {
            return Outcome(left: hosts.map { ($0.title, nil) },
                           advice: "This phone's Keychain couldn't be read. Unlock the phone and reopen the app to try again.")
        }
        outcome.left.reverse()
        return outcome
    }

    /// The computers it could dial, the last used first. Nil when unreadable.
    static func roster(_ defaults: UserDefaults) -> [Host]? {
        guard let saved = decode([Host].self, defaults.data(forKey: "phux.savedServers")) else { return nil }
        let hosts = saved.filter { $0.mode.live != nil }
        // Already most recent first; this names the one last opened.
        let last = decode(Host.self, defaults.data(forKey: "phux.lastServer"))?.id
        return hosts.filter { $0.id == last } + hosts.filter { $0.id != last }
    }

    private static func decode<Value: Decodable>(_ type: Value.Type, _ data: Data?) -> Value? {
        guard let data else { return nil }
        if let stamped = try? JSONDecoder().decode(Envelope<Value>.self, from: data) {
            return stamped.v == 1 ? stamped.data : nil
        }
        return try? JSONDecoder().decode(type, from: data) // Written before the envelope.
    }

    /// What Rune saves for `host`: the same pins, token and device identity,
    /// or a refusal. Nothing is learned, defaulted or left out; `save` then
    /// proves the chain belongs to the key before anything is stored.
    static func configuration(_ host: Host, credential: @MainActor (String) throws -> Data?) throws -> [String: Any] {
        func present(_ value: String?) -> String? {
            value.flatMap { $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : $0 }
        }
        let token = try host.token ?? credential("token.\(host.id)").flatMap { String(data: $0, encoding: .utf8) }
        guard let endpoint = host.mode.live?.url, let token = present(token), let pin = present(host.fingerprint) else {
            throw Refusal(present(host.authority) != nil && present(host.fingerprint) == nil
                ? "it was reached through your tailnet without pairing, which this version can't do"
                : "this phone holds no pairing for it")
        }
        guard let stored = try credential("identity.\(host.id)"), let identity = identity(stored, host: host) else {
            throw Refusal("it was paired before this phone had its own key")
        }
        var values: [String: Any] = ["endpoint": endpoint, "certificate_pin": pin, "token": token,
                                     "chain_pem": identity.chain, "key_id": identity.keyID]
        if let authority = present(host.authority) { values["authority_ca"] = authority }
        if let route = present(host.relayRoute) {
            guard values["authority_ca"] != nil else {
                throw Refusal("it was reached through a relay without its certificate authority")
            }
            values["tls_server_name"] = route
        }
        return values
    }

    private static func identity(_ data: Data, host: Host) -> Identity? {
        let identity: Identity
        if let stored = try? JSONDecoder().decode(Identity.self, from: data) { identity = stored }
        else if let chain = String(data: data, encoding: .utf8), chain.contains("-----BEGIN CERTIFICATE-----") {
            identity = Identity(chain: chain, keyID: host.id) // The key took the roster's id then.
        } else { return nil }
        return UUID(uuidString: identity.keyID) == nil ? nil : identity
    }

    static func keychain(_ account: String) throws -> Data? {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "phux.servers", kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: false,
            kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne]
        var result: CFTypeRef?
        switch SecItemCopyMatching(query as CFDictionary, &result) {
        case errSecSuccess: return result as? Data
        case errSecItemNotFound: return nil
        case let status: throw Unreadable(status: status)
        }
    }

    fileprivate static func list(_ left: [(name: String, reason: String?)]) -> String {
        ListFormatter.localizedString(byJoining: left.map(\.name))
    }

    /// The notice until the person has dismissed it.
    static var notice: String? {
        get { UserDefaults.standard.string(forKey: "earlier-hosts-notice") }
        set { UserDefaults.standard.set(newValue, forKey: "earlier-hosts-notice") }
    }
}
