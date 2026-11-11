#if os(macOS)
import Foundation
import XCTest
@testable import RuneTailnet

final class ScriptTests: XCTestCase {
    func testLocateOnlyFindsExistingExecutable() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let environment = ["HOME": root.path, "PATH": root.appendingPathComponent("empty-bin").path,
                           "PHUX_LOCATE_ROOT": root.path]
        let missing = try run(PairingScripts.locate, environment: environment)
        XCTAssertEqual(missing.0, 127)
        XCTAssertNil(PairingScripts.answer(in: missing.1))
        let bin = root.appendingPathComponent(".local/bin")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let phux = bin.appendingPathComponent("phux")
        FileManager.default.createFile(atPath: phux.path, contents: Data("#!/bin/sh\nexit 0\n".utf8),
                                       attributes: [.posixPermissions: 0o700])
        let found = try run(PairingScripts.locate, environment: environment)
        XCTAssertEqual(found.0, 0)
        XCTAssertEqual(PairingScripts.answer(in: found.1), phux.path + "\n")
    }

    func testPairUsesPOSIXQuotingPrivateUmaskAndOnlyExplicitCommand() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let phux = root.appendingPathComponent("phux with 'quotes'")
        let fake = "#!/bin/sh\numask\nprintf '<%s>\\n' \"$@\"\n"
        FileManager.default.createFile(atPath: phux.path, contents: Data(fake.utf8),
                                       attributes: [.posixPermissions: 0o700])
        let name = "mini'; touch \(root.path)/injected; echo '"
        let result = try run(PairingScripts.pair(phux: phux.path, name: name), environment: ["HOME": root.path])
        XCTAssertEqual(result.0, 0)
        let output = try XCTUnwrap(PairingScripts.answer(in: result.1))
        XCTAssertEqual(output, "0077\n<pair>\n<--json>\n<--enroll>\n<--name>\n<\(name)>\n")
        XCTAssertFalse(FileManager.default.fileExists(atPath: root.appendingPathComponent("injected").path))
    }

    private func run(_ script: String, environment: [String: String]) throws -> (Int32, String) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = ["-s"]
        process.environment = environment
        let input = Pipe()
        let output = Pipe()
        process.standardInput = input
        process.standardOutput = output
        process.standardError = output
        try process.run()
        input.fileHandleForWriting.write(Data(script.utf8))
        try input.fileHandleForWriting.close()
        let bytes = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return (process.terminationStatus, String(decoding: bytes, as: UTF8.self))
    }
}
#endif
