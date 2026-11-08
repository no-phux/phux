import UIKit

// Compiled only for the simulator: no device build can execute a command
// from launch arguments or connect to an arbitrary host filesystem socket.
@MainActor
final class QualificationProbe {
    let configuration: [String: Any]
    let enrollmentTicket: String
    private let session: String
    private let command: String
    private let marker: String
    private var injected = false
    private var finished = false
    private var backgroundFrames: UInt64?
    private var resumed = false
    private var inputContracts: [String] = []

    func testInputContracts() throws {
        #if targetEnvironment(simulator)
        inputContracts = try InputContractTests.run()
        #endif
    }

    init?(arguments: [String]) {
        #if targetEnvironment(simulator)
        func value(_ flag: String) -> String? {
            guard let at = arguments.firstIndex(of: flag), at + 1 < arguments.count else { return nil }
            return arguments[at + 1]
        }
        guard let filename = value("--qualification-configuration"), filename == "qualification-connection.json",
              let session = value("--qualification-session"),
              let command = value("--qualification-input"),
              let marker = value("--qualification-marker"), !marker.isEmpty else { return nil }
        do {
            let directory = try FileManager.default.url(for:.documentDirectory,in:.userDomainMask,
                                                        appropriateFor:nil,create:false)
            let path = directory.appendingPathComponent(filename)
            let bytes = try Data(contentsOf:path)
            try FileManager.default.removeItem(at:path)
            guard var configuration = try JSONSerialization.jsonObject(with:bytes) as? [String:Any],
                  let ticket = configuration.removeValue(forKey:"enrollment_ticket") as? String else { return nil }
            self.enrollmentTicket = ticket
            self.configuration = configuration
        } catch { return nil }
        self.session = session; self.command = command; self.marker = marker
        #else
        return nil
        #endif
    }

    func observe(bridge: NativeBridge, input: RuneInput) throws {
        if resumed, let backgroundFrames {
            let report = try bridge.report(marker:marker)
            let frames = report["frames"] as? UInt64 ?? 0
            if frames > backgroundFrames {
                try write(["schema":1,"background_frames":backgroundFrames,"resumed_frames":frames,
                    "marker_seen":report["marker_seen"] as? Bool ?? false],name:"rune-lifecycle.json")
                self.backgroundFrames = nil
            }
        }
        guard !finished else { return }
        var report = try bridge.report(marker: marker)
        if let message = report["error"] as? String, !message.isEmpty {
            failure(message); finished = true; return
        }
        if !injected, report["connected"] as? Bool == true,
           report["session"] as? String == session,
           (report["frames"] as? UInt64 ?? 0) > 0,
           input.context?.kind == "terminal", input.context?.wanted == true, input.isFirstResponder {
            // This is the same UIKit override used by the system keyboard.
            // It is not an OS-generated keystroke/IME qualification claim.
            injected = true
            input.insertText(command)
            input.insertText("\n")
        }
        guard injected, report["marker_seen"] as? Bool == true else { return }
        report["input_through_native"] = true
        report["transport"] = "paired-quic"
        report["input_contracts"] = inputContracts
        report["input_evidence"] = "UIKit insertText entry; not physical OS typing or IME automation"
        try write(report, name: "rune-qualification.json")
        try write(bridge.tree(), name: "rune-tree.json")
        finished = true
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
