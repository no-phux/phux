import UIKit

#if targetEnvironment(simulator)
@MainActor
enum EarlierAppContractTests {
    private struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static func require(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
        if try !value() { throw Failure(message: message) }
    }

    static func run() throws -> [String] {
        try scenes()
        try hosts()
        try refusals()
        try heldKey()
        return ["earlier-app-scenes", "earlier-app-hosts"]
    }

    /// A session another app's class would open goes; our own stays.
    private static func scenes() throws {
        let files = FileManager.default
        let library = files.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? files.removeItem(at: library) }
        let known = library.appending(path: "Saved Application State/app.savedState/KnownSceneSessions")
        func saved(by delegate: String) throws -> Bool {
            try files.createDirectory(at: known, withIntermediateDirectories: true)
            let data = try NSKeyedArchiver.archivedData(withRootObject: delegate as NSString, requiringSecureCoding: true)
            try data.write(to: known.appending(path: "data.data"))
            EarlierApp.forgetScenes(library: library, bundle: "app")
            return files.fileExists(atPath: known.path)
        }
        try require(try !saved(by: "SwiftUI.AppSceneDelegate"), "an earlier app's scene session must be forgotten")
        try require(try saved(by: NSStringFromClass(SceneDelegate.self)), "our own scene session must stay")
    }

    private static let pin = String(repeating: "AA:", count: 31) + "AA"
    private static let authority = "sha256:" + String(repeating: "b", count: 64)
    private static let secret = String(repeating: "d", count: 64)

    /// One roster entry as the earlier app encoded it.
    private static func host(_ name: String, _ fields: [String: String] = [:], drop: Set<String> = []) -> [String: Any] {
        var host: [String: Any] = ["id": name, "name": name, "fingerprint": pin, "authority": authority,
                                   "mode": ["live": ["url": "quic://\(name).example:8788"]]]
        host.merge(fields) { $1 }
        return host.filter { !drop.contains($0.key) }
    }
    private static let keyID = "852648D8-7BF0-4F8A-A464-78418B29A3F6"
    private static func credentials(_ account: String) -> Data? {
        account.hasPrefix("token.") ? Data(secret.utf8)
            : Data(#"{"chain":"CHAIN","keyID":"\#(keyID)"}"#.utf8)
    }

    /// Carry from a throwaway store, saving into `saved` instead of the Keychain.
    private static func carry(_ roster: Any, last: [String: Any]? = nil, paired: Bool = false,
                              credential: @MainActor (String) throws -> Data? = { credentials($0) })
        throws -> (EarlierHosts.Outcome?, saved: [(values: [String: Any], name: String)]) {
        let suite = "earlier-hosts-contract"
        guard let defaults = UserDefaults(suiteName: suite) else { throw Failure(message: "no test defaults") }
        defaults.removePersistentDomain(forName: suite)
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.set(try JSONSerialization.data(withJSONObject: roster), forKey: "phux.savedServers")
        if let last {
            defaults.set(try JSONSerialization.data(withJSONObject: ["v": 1, "data": last]), forKey: "phux.lastServer")
        }
        var saved: [(values: [String: Any], name: String)] = []
        let outcome = EarlierHosts.carry(defaults: defaults, paired: paired, credential: credential) { saved.append(($0, $1)) }
        return (outcome, saved)
    }

    private static func hosts() throws {
        let both = [host("first"), host("second")]
        let (outcome, saved) = try carry(["v": 1, "data": both], last: both[1])
        try require(saved.count == 1 && saved[0].name == "second" && outcome?.carried == "second",
                    "the last used computer is the one carried")
        try require(saved[0].values as? [String: String] == [
            "endpoint": "quic://second.example:8788", "certificate_pin": pin, "authority_ca": authority,
            "token": secret, "chain_pem": "CHAIN", "key_id": keyID],
            "pins, token and device identity carry unchanged")
        try NativeBridge.validate(saved[0].values)
        try require(outcome?.notice?.contains("first was not carried over") == true, "the other computer is named")
        let (single, only) = try carry([host("only")]) // Before the envelope.
        try require(only.count == 1 && single?.notice == nil, "one computer carries with nothing to say")
        try require(try carry([host("only")], paired: true).saved.isEmpty, "a paired computer is never replaced")
        try require(try carry(["v": 1, "data": [["id": "demo", "name": "demo", "mode": ["demo": [:]]]]]).0 == nil,
                    "a demo is not a computer")
        let routed = try carry([host("relay", ["relayRoute": "mac.route"])]).saved
        try require(routed.first?.values["tls_server_name"] as? String == "mac.route", "a relay route carries")
        // The key took the roster's id before chains were stored with one.
        let legacy = try carry([host("pem", ["id": keyID])]) {
            $0.hasPrefix("token.") ? Data(secret.utf8) : Data("-----BEGIN CERTIFICATE-----".utf8)
        }.saved
        try require(legacy.first?.values["key_id"] as? String == keyID, "an older identity names its key")
    }

    /// Never with less than it was paired with, and never in silence.
    private static func refusals() throws {
        func refused(_ roster: Any, _ words: String,
                     credential: @MainActor (String) throws -> Data? = { credentials($0) }) throws {
            let (outcome, saved) = try carry(roster, credential: credential)
            try require(saved.isEmpty && outcome?.carried == nil, "must not carry: \(words)")
            try require(outcome?.refusal.contains(words) == true, "must say: \(words)")
        }
        try refused([host("mac", drop: ["authority", "fingerprint"])], "this phone holds no pairing for it")
        try refused([host("mac", drop: ["fingerprint"])], "reached through your tailnet without pairing")
        try refused([host("mac")], "this phone holds no pairing for it") { $0.hasPrefix("token.") ? nil : credentials($0) }
        try refused([host("mac")], "before this phone had its own key") { $0.hasPrefix("token.") ? credentials($0) : nil }
        try refused([host("mac", ["relayRoute": "mac.route"], drop: ["authority"])], "without its certificate authority")
        try refused([host("mac")], "Keychain couldn't be read") { _ in throw EarlierHosts.Unreadable(status: -25308) }
        try refused(["v": 2, "data": []], "the computers you had paired")
        // A computer that cannot come does not keep the next one from coming.
        let (outcome, saved) = try carry([host("a", drop: ["fingerprint"]), host("b")])
        try require(saved.first?.name == "b" && outcome?.notice?.contains("a: it was reached") == true,
                    "the next computer is carried, with the reason the first was not")
    }

    /// A carried identity signs with the earlier app's key where it stands.
    private static func heldKey() throws {
        let id = UUID().uuidString
        let tag = Data((NativeIdentity.earlierKeyTag + id).utf8)
        let attributes: [String: Any] = [kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256, kSecUseDataProtectionKeychain as String: true,
            kSecPrivateKeyAttrs as String: [kSecAttrIsPermanent as String: true, kSecAttrApplicationTag as String: tag,
                kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly] as [String: Any]]
        try require(SecKeyCreateRandomKey(attributes as CFDictionary, nil) != nil, "no test key")
        let query: [String: Any] = [kSecClass as String: kSecClassKey, kSecAttrApplicationTag as String: tag,
                                    kSecUseDataProtectionKeychain as String: true]
        defer { SecItemDelete(query as CFDictionary) }
        _ = try NativeIdentity(configuration: ["key_id": id, "chain_pem": "CHAIN"])
        try NativeIdentity.discard(configuration: ["key_id": id])
        try require(SecItemCopyMatching(query as CFDictionary, nil) == errSecSuccess, "the earlier key must never be deleted")
    }
}
#endif
