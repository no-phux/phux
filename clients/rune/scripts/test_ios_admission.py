"""Execute native admission methods with inert pasteboard/Keychain adapters."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SOURCES = Path(__file__).resolve().parents[1] / "ios/Sources"


def member(source, signature):
    """Keep the shipped method body, including its nested control flow."""
    start = source.index(signature)
    end = source.index("\n    }", start) + len("\n    }")
    return source[start:end]


@unittest.skipUnless(sys.platform == "darwin", "Swift execution needs Xcode")
class NativeAdmissionTests(unittest.TestCase):
    def execute(self, source):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            main = root / "main.swift"
            binary = root / "admission"
            main.write_text(source)
            compile_result = subprocess.run(
                ["/usr/bin/xcrun", "--sdk", "macosx", "swiftc",
                 "-swift-version", "6", "-warnings-as-errors", str(main), "-o", str(binary)],
                capture_output=True, text=True, timeout=120,
            )
            self.assertEqual(compile_result.returncode, 0, compile_result.stderr)
            result = subprocess.run([str(binary)], capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_pasted_images_obey_the_file_admission_limit(self):
        source = (SOURCES / "Attachments.swift").read_text()
        methods = "\n".join(member(source, signature) for signature in [
            "static func pasted()", "nonisolated static func file(_ data:",
        ])
        limit = next(line for line in source.splitlines() if "static let maximumBytes =" in line)
        file_type = member(source, "struct File:")
        self.execute("""
import Foundation

struct UTType: Equatable {
    let identifier: String
    static let png = Self(identifier: "png")
    static let jpeg = Self(identifier: "jpg")
    static let gif = Self(identifier: "gif")
    static let webP = Self(identifier: "webp")
    static let svg = Self(identifier: "svg")
    static let image = Self(identifier: "image")
    init(identifier: String) { self.identifier = identifier }
    init?(filenameExtension: String) { self.identifier = filenameExtension }
    func conforms(to other: Self) -> Bool {
        self == other || (other == .image && [.png, .jpeg, .gif, .webP, .svg].contains(self))
    }
}
final class UIImage {
    let jpeg: Data
    init(jpeg: Data) { self.jpeg = jpeg }
    init?(data: Data) { return nil }
    func jpegData(compressionQuality: Double) -> Data? { jpeg }
}
@MainActor final class UIPasteboard {
    static let general = UIPasteboard()
    var hasImages = true
    var values: [String: Data] = [:]
    var image: UIImage?
    func data(forPasteboardType type: String) -> Data? { values[type] }
}
@MainActor enum Attachments {
""" + limit + "\n" + file_type + "\n" + methods + """
}
let board = UIPasteboard.general
let cap = Attachments.maximumBytes
precondition(cap == 48 * 1024 * 1024, "phone-side admission contract")
let oversized = Data(repeating: 1, count: cap + 1)
board.image = UIImage(jpeg: oversized)
precondition(Attachments.pasted() == nil, "fallback JPEG must obey the cap")
board.image = UIImage(jpeg: Data())
precondition(Attachments.pasted() == nil, "empty JPEG is not a file")
board.values["png"] = Data()
precondition(Attachments.pasted() == nil, "empty direct bytes are not a file")
board.values["png"] = oversized
board.image = UIImage(jpeg: Data([1, 2]))
precondition(Attachments.pasted()?.data == Data([1, 2]), "bounded fallback is allowed")
precondition(Attachments.pasted()?.fileExtension == "jpg")
board.values["png"] = Data(repeating: 1, count: cap)
precondition(Attachments.pasted()?.data.count == cap, "cap is inclusive")
precondition(Attachments.pasted()?.fileExtension == "png")
board.hasImages = false
precondition(Attachments.pasted() == nil)
""")

    def test_failed_saved_host_read_preserves_the_current_computer(self):
        source = (SOURCES / "ConnectionForm.swift").read_text()
        methods = [
            member(source, "static var computers:"),
            member(source, "static func select("),
            member(source, "static func load()"),
        ]
        if "private static func load(_" in source:
            methods.append(member(source, "private static func load(_"))
        # In-memory defaults and a fake Security API ensure these tests
        # never touch the user's saved hosts, Keychain, credentials or device.
        methods = "\n".join(methods).replace("UserDefaults.standard", "defaults")
        self.execute("""
import Foundation
import CoreFoundation

enum NativeFailure: Error { case message(String) }
let kSecClass = "class", kSecClassGenericPassword = "password"
let kSecAttrService = "service", kSecAttrAccount = "account"
let kSecReturnData = "data", kSecMatchLimit = "limit", kSecMatchLimitOne = "one"
let errSecSuccess: Int32 = 0, errSecItemNotFound: Int32 = -25300
var keychainStatus: Int32 = -25308
var credential = Data("selected-token".utf8)
@MainActor func SecItemCopyMatching(_ query: CFDictionary,
                                    _ result: UnsafeMutablePointer<CFTypeRef?>) -> Int32 {
    result.pointee = credential as CFData
    return keychainStatus
}
@MainActor final class MemoryDefaults {
    var values: [String: Any] = [:]
    func set(_ value: Any, forKey key: String) { values[key] = value }
    func removeObject(forKey key: String) { values.removeValue(forKey: key) }
    func dictionary(forKey key: String) -> [String: Any]? { values[key] as? [String: Any] }
    func array(forKey key: String) -> [Any]? { values[key] as? [Any] }
    func string(forKey key: String) -> String? { values[key] as? String }
}
@MainActor enum HostSettings {
    static let service = "test"
    static let nameKey = "paired-computer-name"
    static let computersKey = "paired-computers"
    static let defaults = MemoryDefaults()
""" + methods + """
}
let defaults = HostSettings.defaults
let old = ["endpoint": "quic://old", "certificate_pin": "old-pin"]
let next = ["endpoint": "quic://next", "certificate_pin": "next-pin", "name": "Next"]
defaults.set(old, forKey: "remote-host")
defaults.set("Old", forKey: HostSettings.nameKey)
defaults.set([next], forKey: HostSettings.computersKey)
do {
    _ = try HostSettings.select("quic://next")
    preconditionFailure("Keychain failure must propagate")
} catch {}
precondition(defaults.dictionary(forKey: "remote-host") as? [String: String] == old,
             "failure must not select another host")
precondition(defaults.string(forKey: HostSettings.nameKey) == "Old")
keychainStatus = errSecSuccess
credential = Data([0xff])
do { _ = try HostSettings.select("quic://next"); preconditionFailure("invalid UTF-8") } catch {}
precondition(defaults.dictionary(forKey: "remote-host") as? [String: String] == old)
precondition(defaults.string(forKey: HostSettings.nameKey) == "Old")
credential = Data("selected-token".utf8)
let selected = try HostSettings.select("quic://next")
precondition(selected?["token"] as? String == "selected-token")
precondition(selected?["name"] == nil, "display name is not a config field")
precondition(defaults.dictionary(forKey: "remote-host")?["token"] == nil)
precondition(defaults.dictionary(forKey: "remote-host")?["endpoint"] as? String == "quic://next")
precondition(defaults.string(forKey: HostSettings.nameKey) == "Next")
let reloaded = try HostSettings.load()
precondition(reloaded?["token"] as? String == "selected-token")
let before = defaults.dictionary(forKey: "remote-host") as? [String: String]
let missing = try HostSettings.select("quic://missing")
precondition(missing == nil)
precondition(defaults.dictionary(forKey: "remote-host") as? [String: String] == before)
""")
