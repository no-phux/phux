import UIKit

// Compiled only for the simulator: no device build can execute a command
// from launch arguments or connect to an arbitrary host filesystem socket.
@MainActor
final class QualificationProbe {
    private let pairingLink: String?
    private let resuming: Bool
    let connectionEndpoint: String?
    private let session: String
    private let command: String
    private let marker: String
    private var injected = false
    private var finished = false
    private var backgroundFrames: UInt64?
    private var resumed = false
    private var inputContracts: [String] = []
    private var imported = false
    private var keyID: String?
    private var connectedEndpoint: String?

    func testInputContracts() async throws {
        #if targetEnvironment(simulator)
        inputContracts = try InputContractTests.run()
        inputContracts += try await PairingContractTests.run()
        #endif
    }

    func prepareConnection() async throws -> [String: Any] {
        #if targetEnvironment(simulator)
        if resuming {
            guard let saved = try HostSettings.load() else { throw NativeFailure.message("No saved identity to resume.") }
            try NativeIdentity(configuration: saved).validate(configuration: saved)
            keyID = saved["key_id"] as? String
            connectedEndpoint = saved["endpoint"] as? String
            return saved
        }
        guard let pairingLink else { throw NativeFailure.message("Missing qualification pairing link.") }
        let endpoint = connectionEndpoint, session = session
        let flow = PairingFlow(enroll: { invite in
            var issued = try await NativeIdentity.enroll(configuration: invite.configuration, ticket: invite.ticket)
            if let endpoint { issued["endpoint"] = endpoint }
            issued["session"] = session
            return issued
        })
        var saved: [String: Any]?
        flow.connected = { saved = $0 }
        flow.receive(pairingLink)
        guard case .review = flow.stage else { throw NativeFailure.message("Pairing payload import failed.") }
        flow.confirm() // Same explicit confirmation operation as the native button.
        await flow.qualificationOperation?.value
        guard let saved else { throw NativeFailure.message(flow.message ?? "Pairing did not connect.") }
        imported = true
        keyID = saved["key_id"] as? String
        connectedEndpoint = saved["endpoint"] as? String
        return saved
        #else
        throw NativeFailure.message("Qualification is unavailable on a device.")
        #endif
    }

    init?(arguments: [String]) {
        #if targetEnvironment(simulator)
        func value(_ flag: String) -> String? {
            guard let at = arguments.firstIndex(of: flag), at + 1 < arguments.count else { return nil }
            return arguments[at + 1]
        }
        guard let session = value("--qualification-session"),
              let command = value("--qualification-input"),
              let marker = value("--qualification-marker"), !marker.isEmpty else { return nil }
        resuming = arguments.contains("--qualification-resume")
        if resuming {
            pairingLink = nil; connectionEndpoint = nil
            self.session = session; self.command = command; self.marker = marker
            return
        }
        guard let filename = value("--qualification-configuration"), filename == "qualification-connection.json" else { return nil }
        do {
            let configuration = try Self.readFixture(filename)
            guard let link = configuration["pairing_link"] as? String else { return nil }
            self.pairingLink = link
            self.connectionEndpoint = configuration["qualification_endpoint"] as? String
        } catch { return nil }
        self.session = session; self.command = command; self.marker = marker
        #else
        return nil
        #endif
    }

    private static func readFixture(_ filename: String) throws -> [String: Any] {
        let directory = try FileManager.default.url(for: .documentDirectory, in: .userDomainMask,
                                                     appropriateFor: nil, create: false)
        let path = directory.appendingPathComponent(filename)
        let bytes = try Data(contentsOf: path)
        try FileManager.default.removeItem(at: path)
        guard let value = try JSONSerialization.jsonObject(with: bytes) as? [String: Any] else {
            throw NativeFailure.message("Invalid qualification fixture.")
        }
        return value
    }

    func observe(bridge: NativeBridge, input: RuneInput) throws {
        try recordResumedFrame(bridge)
        guard !finished else { return }
        var report = try bridge.report(marker: marker)
        if let message = report["error"] as? String, !message.isEmpty {
            failure(message); finished = true; return
        }
        injectInput(report, input: input)
        guard injected, report["marker_seen"] as? Bool == true else { return }
        report["input_through_native"] = true
        report["transport"] = connectedEndpoint?.hasPrefix("wss://") == true ? "paired-wss" : "paired-quic"
        report["onboarding_import"] = imported
        report["onboarding_confirmed"] = imported
        report["resumed_identity"] = resuming
        report["key_id"] = keyID
        report["input_contracts"] = inputContracts
        report["input_evidence"] = "UIKit insertText entry; not physical OS typing or IME automation"
        try write(report, name: "rune-qualification.json")
        try write(bridge.tree(), name: "rune-tree.json")
        finished = true
    }

    private func recordResumedFrame(_ bridge: NativeBridge) throws {
        guard resumed, let backgroundFrames else { return }
        let report = try bridge.report(marker: marker)
        let frames = report["frames"] as? UInt64 ?? 0
        guard frames > backgroundFrames else { return }
        try write(["schema": 1, "background_frames": backgroundFrames, "resumed_frames": frames,
                   "marker_seen": report["marker_seen"] as? Bool ?? false], name: "rune-lifecycle.json")
        self.backgroundFrames = nil
    }

    private func injectInput(_ report: [String: Any], input: RuneInput) {
        guard !injected, report["connected"] as? Bool == true,
              report["session"] as? String == session,
              (report["frames"] as? UInt64 ?? 0) > 0,
              input.context?.kind == "terminal", input.context?.wanted == true, input.isFirstResponder else { return }
        // The same UIKit override used by the system keyboard; not an
        // OS-generated keystroke/IME qualification claim.
        injected = true
        input.insertText(command)
        input.insertText("\n")
    }

    func active(_ active:Bool, bridge:NativeBridge) {
        if active { resumed = backgroundFrames != nil }
        else if let report = try? bridge.report(marker:marker) {
            backgroundFrames = report["frames"] as? UInt64
            if let backgroundFrames {
                do { try write(["schema":1,"background_frames":backgroundFrames],name:"rune-background.json") }
                catch { failure(error.localizedDescription) }
            }
        }
    }

    func failure(_ message: String) {
        try? write(["schema":1, "error":message], name:"rune-qualification.json")
    }
    private func write(_ object: [String: Any], name: String) throws {
        let directory = try FileManager.default.url(for: .documentDirectory, in: .userDomainMask,
                                                    appropriateFor: nil, create: true)
        let bytes = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted,.sortedKeys])
        try bytes.write(to: directory.appendingPathComponent(name), options: .atomic)
    }
}
