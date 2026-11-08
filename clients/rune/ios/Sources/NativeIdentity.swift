import Foundation
import Security

private struct IdentityFailure: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

// SecKey is immutable and supports concurrent signing. No callback touches
// UIKit or an actor. Rust's Arc retains this context independently of the
// wrapper, so an outstanding handshake remains valid after the view closes.
private final class KeySigner: @unchecked Sendable {
    let key: SecKey
    init(_ key: SecKey) { self.key = key }

    func publicPoint() throws -> Data {
        var error: Unmanaged<CFError>?
        guard let publicKey = SecKeyCopyPublicKey(key),
              let point = SecKeyCopyExternalRepresentation(publicKey, &error) else {
            if let error { _ = error.takeRetainedValue() }
            throw IdentityFailure(message: "Could not read the device key's public point.")
        }
        return point as Data
    }

    func sign(_ message: Data) -> Data? {
        var error: Unmanaged<CFError>?
        let result = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256,
                                          message as CFData, &error)
        if let error { _ = error.takeRetainedValue() }
        return result as Data?
    }
}

nonisolated private let signDeviceMessage: @convention(c) (
    UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int, UnsafeMutablePointer<UInt8>?, Int
) -> Int = { context, message, length, output, capacity in
    guard let context, let message, let output, length >= 0, capacity > 0 else { return -1 }
    let signer = Unmanaged<KeySigner>.fromOpaque(context).takeUnretainedValue()
    guard let signature = signer.sign(Data(bytes: message, count: length)),
          signature.count <= capacity else { return -1 }
    signature.copyBytes(to: output, count: signature.count)
    return signature.count
}

nonisolated private let releaseDeviceSigner: @convention(c) (UnsafeMutableRawPointer?) -> Void = { context in
    if let context { Unmanaged<KeySigner>.fromOpaque(context).release() }
}

/// Only the public chain/tag crosses the bridge. The private key is never
/// exported, including on the simulator (which uses a Keychain SecKey).
final class NativeIdentity: @unchecked Sendable {
    let handle: UnsafeMutableRawPointer
    private static let cleanupLock = NSLock()
    private static let cleanupPreference = "pending-device-key-cleanup"

    init(configuration: [String: Any]) throws {
        guard let keyID = configuration["key_id"] as? String, UUID(uuidString: keyID) != nil,
              let chain = configuration["chain_pem"] as? String, !chain.isEmpty else {
            throw IdentityFailure(message: "Enroll this phone before opening its remote host.")
        }
        let signer = KeySigner(try Self.loadKey(keyID))
        handle = try Self.makeHandle(signer)
    }

    private init(newKeyID: String) throws {
        handle = try Self.makeHandle(KeySigner(Self.createKey(newKeyID)))
    }

    deinit { phux_rune_ios_identity_destroy(handle) }

    private static func makeHandle(_ signer: KeySigner) throws -> UnsafeMutableRawPointer {
        let point = try signer.publicPoint()
        let context = Unmanaged.passRetained(signer).toOpaque()
        let handle = point.withUnsafeBytes { bytes in
            phux_rune_ios_identity_create(bytes.bindMemory(to: UInt8.self).baseAddress,
                                          point.count, context, signDeviceMessage, releaseDeviceSigner)
        }
        // Rust consumes context even when the point is rejected.
        guard let handle else { throw IdentityFailure(message: bridgeError()) }
        return handle
    }

    func validate(configuration: [String: Any]) throws {
        let data = try JSONSerialization.data(withJSONObject: configuration)
        _ = try String(decoding: data, as: UTF8.self).withCString {
            try Self.object(phux_rune_ios_identity_validate($0, handle))
        }
    }

    /// The dictionary remains on the main actor. Only immutable JSON, ticket
    /// and Sendable result strings enter/leave the blocking background task.
    @MainActor
    static func enroll(configuration: [String: Any], ticket: String) async throws -> [String: Any] {
        try Task.checkCancellation()
        let data = try JSONSerialization.data(withJSONObject: configuration)
        let json = String(decoding: data, as: UTF8.self)
        _ = try json.withCString { try object(phux_rune_ios_enrollment_validate($0)) }
        let ticket = ticket.trimmingCharacters(in: .whitespacesAndNewlines)
        guard ticket.utf8.count == 64, ticket.utf8.allSatisfy({ $0.isHexDigit }) else {
            throw IdentityFailure(message: "The enrollment ticket must contain 64 hex digits.")
        }
        let issued = try await Task.detached(priority: .userInitiated) {
            try blockingEnroll(json: json, ticket: ticket)
        }.value
        if Task.isCancelled {
            do { try deleteKey(issued.keyID) }
            catch { throw IdentityFailure(message:"Enrollment cancelled. \(error.localizedDescription)") }
            throw CancellationError()
        }
        var values = configuration
        values["chain_pem"] = issued.chain
        values["key_id"] = issued.keyID
        return values
    }

    private struct Issued: Sendable { let chain: String; let keyID: String }

    private static func blockingEnroll(json: String, ticket: String) throws -> Issued {
        // Never re-enroll a registered key (including a revoked key). A fresh
        // tag also leaves any saved, working identity untouched on failure.
        let keyID = UUID().uuidString
        do {
            let identity = try NativeIdentity(newKeyID: keyID)
            let response: [String:Any] = try json.withCString { configuration in
                try ticket.withCString { ticket in
                    try Self.object(phux_rune_ios_enroll(configuration, ticket, identity.handle))
                }
            }
            guard let chain = response["chain_pem"] as? String, !chain.isEmpty else {
                throw IdentityFailure(message: "Enrollment returned no certificate chain.")
            }
            return Issued(chain: chain, keyID: keyID)
        } catch {
            let enrollmentError = error
            do { try deleteKey(keyID) }
            catch {
                throw IdentityFailure(message:"\(enrollmentError.localizedDescription) \(error.localizedDescription)")
            }
            throw enrollmentError
        }
    }

    static func discard(configuration: [String: Any]) throws {
        guard let keyID = configuration["key_id"] as? String, UUID(uuidString: keyID) != nil else {
            throw IdentityFailure(message: "The staged device key tag is invalid.")
        }
        try deleteKey(keyID)
    }

    private static func keyQuery(_ keyID: String) -> [String: Any] {
        [kSecClass as String: kSecClassKey,
         kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
         kSecAttrKeyClass as String: kSecAttrKeyClassPrivate,
         kSecAttrApplicationTag as String: Data("sh.phux.rune.ios.device.\(keyID)".utf8),
         kSecUseDataProtectionKeychain as String: true]
    }

    private static func loadKey(_ keyID: String) throws -> SecKey {
        var query = keyQuery(keyID)
        query[kSecReturnRef as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess else {
            throw IdentityFailure(message: "Could not open the enrolled device key (Keychain \(status)). Unlock this phone or enroll again.")
        }
        guard let result, CFGetTypeID(result) == SecKeyGetTypeID() else {
            throw IdentityFailure(message: "The enrolled device key is not a Security.framework key.")
        }
        return result as! SecKey
    }

    private static func createKey(_ keyID: String) throws -> SecKey {
        var error: Unmanaged<CFError>?
        var privateAttributes: [String: Any] = [
            kSecAttrIsPermanent as String: true,
            kSecAttrApplicationTag as String: Data("sh.phux.rune.ios.device.\(keyID)".utf8)]
        var attributes: [String: Any] = [kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256, kSecUseDataProtectionKeychain as String: true]
        #if targetEnvironment(simulator)
        privateAttributes[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        #else
        guard let access = SecAccessControlCreateWithFlags(nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
                                                          .privateKeyUsage, &error) else {
            if let error { _ = error.takeRetainedValue() }
            throw IdentityFailure(message: "Could not configure Secure Enclave key access.")
        }
        attributes[kSecAttrTokenID as String] = kSecAttrTokenIDSecureEnclave
        privateAttributes[kSecAttrAccessControl as String] = access
        #endif
        attributes[kSecPrivateKeyAttrs as String] = privateAttributes
        guard let key = SecKeyCreateRandomKey(attributes as CFDictionary, &error) else {
            if let error { _ = error.takeRetainedValue() }
            throw IdentityFailure(message: "Could not create the device enrollment key. Unlock this phone and retry.")
        }
        return key
    }

    private static func deleteKey(_ keyID: String) throws {
        let status = SecItemDelete(keyQuery(keyID) as CFDictionary)
        let removed = status == errSecSuccess || status == errSecItemNotFound
        cleanupLock.withLock {
            var pending = Set(UserDefaults.standard.stringArray(forKey:cleanupPreference) ?? [])
            if removed { pending.remove(keyID) } else { pending.insert(keyID) }
            UserDefaults.standard.set(Array(pending),forKey:cleanupPreference)
        }
        guard removed else {
            throw IdentityFailure(message: "Could not remove the staged device key (Keychain \(status)); cleanup will retry when the app becomes active.")
        }
    }

    /// Store only public UUID tags, never secret keys/tickets. A locked
    /// Keychain must not turn a cancelled enrollment into an unreachable key.
    static func retryPendingCleanup() -> [String] {
        let pending = cleanupLock.withLock {
            UserDefaults.standard.stringArray(forKey:cleanupPreference) ?? []
        }
        return pending.compactMap { keyID in
            guard UUID(uuidString:keyID) != nil else { return "Invalid staged key cleanup tag." }
            do { try deleteKey(keyID); return nil }
            catch { return error.localizedDescription }
        }
    }

    private static func object(_ pointer: UnsafeMutablePointer<CChar>?) throws -> [String: Any] {
        guard let pointer else { throw IdentityFailure(message: bridgeError()) }
        defer { phux_rune_ios_free(pointer) }
        let data = Data(String(cString: pointer).utf8)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw IdentityFailure(message: "Enrollment returned invalid native state.")
        }
        return object
    }

    private static func bridgeError() -> String {
        guard let pointer = phux_rune_ios_error() else { return "The device identity operation failed." }
        defer { phux_rune_ios_free(pointer) }
        let data = Data(String(cString: pointer).utf8)
        let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        return object?["error"] as? String ?? "The device identity operation failed."
    }
}

private extension UInt8 {
    var isHexDigit: Bool { (48...57).contains(self) || (65...70).contains(self) || (97...102).contains(self) }
}
