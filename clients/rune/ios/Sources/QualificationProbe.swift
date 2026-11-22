import UIKit

/// The runner's request to exercise the Liquid Glass controls: a Darwin
/// notification, posted inside the simulator after the terminal screenshot.
private let glassSignal = "sh.phux.rune.qualification.glass"

// Darwin notifications arrive on an arbitrary thread; like a wake, the weak
// probe is read only after hopping to the main queue.
private final class GlassSignalBox: @unchecked Sendable {
    weak var probe: QualificationProbe?
    func schedule() { DispatchQueue.main.async { [self] in probe?.glassRequested() } }
}

// Compiled only for the simulator: no device build can execute a command
// from launch arguments or connect to an arbitrary host filesystem socket.
@MainActor
final class QualificationProbe {
    weak var surface: RuneView?
    private let glassBox = GlassSignalBox()
    /// The terminal's glass controls when the runner asked, until the tapped
    /// back button's Sessions page shows its own.
    private var glassBefore: [[String: Any]]?
    private var glassRecorded = false
    private let pairingLink: String?
    private let resuming: Bool
    let connectionEndpoint: String?
    private let session: String
    private let command: String
    private let marker: String
    private var progress = QualificationProgress()
    private var backgroundFrames: UInt64?
    private var resumed = false
    private var inputContracts: [String] = []
    private var imported = false
    private var keyID: String?
    private var connectedEndpoint: String?
    /// The last input gate written to `rune-input-gate.json`.
    private var inputGate: Bool?

    func testInputContracts() async throws {
        #if targetEnvironment(simulator)
        let directory = try FileManager.default.url(for: .documentDirectory, in: .userDomainMask,
                                                     appropriateFor: nil, create: true)
        let name = (resuming ? "resume-" : "") + "rune-connection-diagnostics.jsonl"
        let captured = directory.appendingPathComponent(name).path.withCString {
            phux_rune_ios_capture_connection_diagnostics($0)
        }
        if !captured { NSLog("Qualification connection diagnostics could not start.") }
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
            observeGlassSignal()
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
        observeGlassSignal()
        #else
        return nil
        #endif
    }

    private func observeGlassSignal() {
        glassBox.probe = self
        let box = Unmanaged.passRetained(glassBox).toOpaque()
        CFNotificationCenterAddObserver(CFNotificationCenterGetDarwinNotifyCenter(), box, { _, observer, _, _, _ in
            guard let observer else { return }
            Unmanaged<GlassSignalBox>.fromOpaque(observer).takeUnretainedValue().schedule()
        }, glassSignal as CFString, nil, .deliverImmediately)
    }

    /// Note the terminal's glass controls, then tap the real back button.
    /// Only after the terminal report: it must not disturb input evidence.
    fileprivate func glassRequested() {
        guard progress.finished, glassBefore == nil, !glassRecorded, let surface else { return }
        let before = surface.glassEvidence
        guard surface.tapGlass("phone-back") else {
            failure("The native Liquid Glass back button could not be tapped."); return
        }
        glassBefore = before
    }

    /// The Sessions page the glass back button opened, once its own glass
    /// dock shows and the covered top bar's glass is gone.
    private func recordGlass() throws {
        guard let before = glassBefore, !glassRecorded, let surface else { return }
        let after = surface.glassEvidence
        let ids = Set(after.compactMap { $0["id"] as? String })
        guard ids.contains("phone-session-new"), !ids.contains("phone-back") else { return }
        try write(["schema": 1, "tapped": "phone-back", "terminal": before, "sessions": after],
                  name: "rune-glass.json")
        glassRecorded = true
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

    func observe(bridge: NativeBridge, input: RuneInput, surfaceAtScale: Bool, didPresent: Bool) throws {
        guard didPresent else { return }
        try recordResumedFrame(bridge)
        try recordGlass()
        guard !progress.finished else { return }
        var report = try bridge.report(marker: marker)
        if let message = report["error"] as? String, !message.isEmpty {
            failure(message); return
        }
        if progress.refusedAfterReady(refusals: report["input_refusals"] as? UInt64 ?? 0) {
            failure("input refused after input_ready"); return
        }
        injectInput(report, input: input, bridge: bridge)
        guard progress.injected, report["marker_seen"] as? Bool == true else { return }
        report["input_through_native"] = true
        report["transport"] = connectedEndpoint?.hasPrefix("wss://") == true ? "paired-wss" : "paired-quic"
        report["onboarding_import"] = imported
        report["onboarding_confirmed"] = imported
        report["resumed_identity"] = resuming
        report["surface_at_scale"] = surfaceAtScale
        report["key_id"] = keyID
        report["input_contracts"] = inputContracts
        report["input_evidence"] = "UIKit insertText entry; not physical OS typing or IME automation"
        try progress.publish {
            try QualificationEvidence.publish(report: report, tree: bridge.tree(), session: session,
                marker: marker, didPresent: didPresent, write: { try self.write($0, name: $1) })
        }
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

    private func injectInput(_ report: [String: Any], input: RuneInput, bridge: NativeBridge) {
        let shown = report["connected"] as? Bool == true &&
              report["session"] as? String == session &&
              (report["frames"] as? UInt64 ?? 0) > 0 &&
              input.context?.kind == "terminal" && input.context?.wanted == true && input.isFirstResponder
        let inputReady = report["input_ready"] as? Bool == true
        if shown && !progress.injected { recordInputGate(inputReady) }
        guard progress.claimInput(shown: shown, inputReady: inputReady,
                                  refusals: report["input_refusals"] as? UInt64 ?? 0) else { return }
        // Same attempt clock as the InputKey send, written before it.
        bridge.traceInputReady()
        // The same UIKit override used by the system keyboard; not an
        // OS-generated keystroke/IME qualification claim.
        input.insertText(command)
        input.insertText("\n")
    }

    /// What the driver names if no report arrives: a frame is shown, and
    /// whether the client has said input would reach it. Written on change.
    private func recordInputGate(_ ready: Bool) {
        guard inputGate != ready else { return }
        inputGate = ready
        try? write(["schema": 1, "frame_shown": true, "input_ready": ready], name: "rune-input-gate.json")
    }

    func active(_ active:Bool, bridge:NativeBridge) {
        if !active { finishConnectionDiagnosticsIfRequested() }
        if active { resumed = backgroundFrames != nil }
        else if let report = try? bridge.report(marker:marker) {
            backgroundFrames = report["frames"] as? UInt64
            if let backgroundFrames {
                do { try write(["schema":1,"background_frames":backgroundFrames],name:"rune-background.json") }
                catch { failure(error.localizedDescription) }
            }
        }
    }

    private func finishConnectionDiagnosticsIfRequested() {
        #if targetEnvironment(simulator)
        guard let directory = try? FileManager.default.url(for: .documentDirectory, in: .userDomainMask,
                                                            appropriateFor: nil, create: false) else { return }
        let name = (resuming ? "resume-" : "") + "rune-connection-diagnostics.finish"
        guard FileManager.default.fileExists(atPath: directory.appendingPathComponent(name).path) else { return }
        // Lifecycle only, never a frame poll. A failure is recorded separately
        // from the authoritative first qualification report.
        if !phux_rune_ios_finish_connection_diagnostics() {
            NSLog("Qualification connection capture did not finish cleanly.")
        }
        #endif
    }

    func preparationFailure(_ message: String) {
        guard progress.claimFailure() else { return }
        #if targetEnvironment(simulator)
        // The awaited enrollment operation has returned, and open() has never
        // run. Publish the original failure only after seal cleanup returns:
        // report visibility then acknowledges finalization without a surface,
        // invented frame counts, or a second OS-lifecycle path.
        QualificationEvidence.preparationFailure(
            seal: { _ = phux_rune_ios_finish_connection_diagnostics() },
            publish: {
                try? self.write(["schema": 1, "error": message, "preparation_failed": true],
                                name: "rune-qualification.json")
            })
        #endif
    }

    func failure(_ message: String) {
        guard progress.claimFailure() else { return }
        try? write(["schema":1, "error":message], name:"rune-qualification.json")
    }
    private func write(_ object: [String: Any], name: String) throws {
        let directory = try FileManager.default.url(for: .documentDirectory, in: .userDomainMask,
                                                    appropriateFor: nil, create: true)
        let bytes = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted,.sortedKeys])
        try bytes.write(to: directory.appendingPathComponent(name), options: .atomic)
    }
}
