import Foundation

/// The shipped probe's one-shot input and terminal outcome, independent of UIKit.
/// A later live connection must not revive a failed qualification.
struct QualificationProgress {
    private(set) var injected = false
    private(set) var finished = false
    private var failed = false

    mutating func claimInput(when ready: Bool) -> Bool {
        guard ready, !finished, !injected else { return false }
        injected = true
        return true
    }

    mutating func publish(_ write: () throws -> Bool) rethrows {
        guard !finished else { return }
        finished = try write()
    }

    mutating func claimFailure() -> Bool {
        guard !failed else { return false }
        failed = true
        finished = true
        return true
    }
}

/// Qualification's readiness file must describe the frame whose tree was saved.
enum QualificationEvidence {
    /// A pre-consumer failure report also acknowledges that synchronous seal
    /// cleanup returned. It is never rendered-input or lifecycle evidence.
    static func preparationFailure(seal: () -> Void, publish: () -> Void) {
        seal()
        publish()
    }

    static func publish(report: [String: Any], tree: [String: Any], marker: String,
                        didPresent: Bool, write: ([String: Any], String) throws -> Void) throws -> Bool {
        guard didPresent, report["marker_seen"] as? Bool == true,
              let frames = report["frames"] as? UInt64, frames > 0,
              let terminal = report["terminal_id"] as? String, !terminal.isEmpty,
              let grid = grid(tree, key: "grid-\(terminal)"),
              contains(marker, in: (grid["children"] as? [[String: Any]] ?? [])
                  .flatMap { labels($0) }.joined()) else { return false }
        var capturedTree = tree
        capturedTree["qualification"] = [
            "schema": 1, "marker": marker, "frames": frames, "terminal_id": terminal
        ]
        var capturedReport = report
        capturedReport["tree_marker_seen"] = true
        capturedReport["tree_frames"] = frames
        // The Python driver treats report existence as readiness. Each write
        // is atomic; a report can never expose an absent or preceding tree.
        try write(capturedTree, "rune-tree.json")
        try write(capturedReport, "rune-qualification.json")
        return true
    }

    private static func grid(_ node: [String: Any], key: String) -> [String: Any]? {
        if node["key"] as? String == key { return node }
        for child in node["children"] as? [[String: Any]] ?? [] {
            if let found = grid(child, key: key) { return found }
        }
        return nil
    }

    private static func labels(_ node: [String: Any]) -> [String] {
        var words: [String] = []
        if node["role"] as? String == "label", let name = node["name"] as? String {
            words.append(name)
        }
        for child in node["children"] as? [[String: Any]] ?? [] {
            words += labels(child)
        }
        return words
    }

    private static func contains(_ marker: String, in words: String) -> Bool {
        guard !marker.isEmpty else { return false }
        // No soft-wrap flag distinguishes rows from styled runs. The command
        // prints a closed witness: even a wrapped RESUMED suffix cannot close
        // the initial phase's witness. A prompt on the next row is harmless.
        return words.replacingOccurrences(of: "\n", with: "").contains("[[" + marker + "]]")
    }
}
