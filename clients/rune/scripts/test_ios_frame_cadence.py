"""Execute the shipped Swift frame/evidence policies, not Python copies."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from test_ios_build import build


@unittest.skipUnless(sys.platform == "darwin", "Swift host execution needs Xcode")
class NativeFramePolicyTests(unittest.TestCase):
    def test_failed_drawables_and_unpresented_evidence_never_claim_success(self):
        source = Path(__file__).resolve().parents[1] / "ios/Sources/FrameCadence.swift"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evidence = source.with_name("QualificationEvidence.swift")
            main = root / "main.swift"
            main.write_text("""
import Foundation
var cadence = FrameCadence()
precondition(cadence.acceptsWake(timerScheduled: true))
for expected in [0.05, 0.1, 0.2, 0.25, 0.25, 0.25, 0.25, 0.25] {
    precondition(cadence.nextWake(drew: false, after: nil) == expected)
    // Ordinary input/network wakes cannot turn a pending retry into 120 Hz
    // acquisition attempts, but a cancelled retry may be rearmed.
    precondition(!cadence.acceptsWake(timerScheduled: true))
    precondition(cadence.acceptsWake(timerScheduled: false))
}
precondition(cadence.nextWake(drew: false, after: 0.01) == 0.01)
precondition(cadence.nextWake(drew: false, after: 10) == 0.25)
precondition(cadence.nextWake(drew: true, after: nil) == nil)
precondition(cadence.nextWake(drew: false, after: nil) == 0.05)
cadence.reset()
precondition(cadence.acceptsWake(timerScheduled: true))
precondition(cadence.nextWake(drew: false, after: .infinity) == 0.05)
precondition(cadence.nextWake(drew: true, after: .nan) == nil)
precondition(cadence.nextWake(drew: true, after: -1) == 0.001)
precondition(cadence.nextWake(drew: true, after: 2) == 2)

let marker = "RUNE_IOS_INPUT_OK"
let witness = "[[" + marker + "]]"
let session = "qualification"
let report: [String: Any] = [
    "connected": true, "session": session, "marker_seen": true, "frames": UInt64(7), "terminal_id": "@1"
]
func tree(_ text: String, grid: String = "grid-@1") -> [String: Any] {
    ["role": "window", "children": [
        ["role": "group", "key": grid, "children": [["role": "label", "name": text]]]
    ]]
}
@MainActor func publish(_ tree: [String: Any], drew: Bool = true,
             source: [String: Any] = report) throws -> [(String, [String: Any])] {
    var writes: [(String, [String: Any])] = []
    _ = try QualificationEvidence.publish(report: source, tree: tree, session: session, marker: marker,
        didPresent: drew, write: { value, name in writes.append((name, value)) })
    return writes
}
// The live grid can be ahead of the frame that Rune actually presented.
let stale = try publish(tree("sh-3.2$"))
precondition(stale.isEmpty, "stale tree must not publish readiness")
let undrawn = try publish(tree(witness), drew: false)
precondition(undrawn.isEmpty, "undrawn frame is not proof")
let wrongTerminal = try publish(tree(witness, grid: "grid-@2"))
precondition(wrongTerminal.isEmpty, "wrong terminal is not proof")
let wrongPhase = try publish(tree("[[" + marker + "_RESUMED]]"))
precondition(wrongPhase.isEmpty, "phase prefix is not proof")
let splitPhase: [String: Any] = ["role": "group", "key": "grid-@1", "children": [
    ["role": "label", "name": "[[" + marker], ["role": "label", "name": "_RESUMED]]"]
]]
let splitWrongPhase = try publish(splitPhase)
precondition(splitWrongPhase.isEmpty, "a wrapped phase suffix cannot make the initial phase ready")
let bareMarker = try publish(tree(marker))
precondition(bareMarker.isEmpty, "only the command's closed witness is proof")
let metadataOnly = try publish(["name": witness, "children": []])
precondition(metadataOnly.isEmpty, "metadata is not terminal text")
let saved = try publish(tree(witness))
precondition(saved.map { $0.0 } == ["rune-tree.json", "rune-qualification.json"],
    "readiness must be published last")
let treeEvidence = saved[0].1["qualification"] as! [String: Any]
precondition(treeEvidence["marker"] as? String == marker)
precondition(treeEvidence["frames"] as? UInt64 == 7)
precondition(treeEvidence["terminal_id"] as? String == "@1")
precondition(saved[1].1["tree_marker_seen"] as? Bool == true)
precondition(saved[1].1["tree_frames"] as? UInt64 == 7)
let wrapped: [String: Any] = ["role": "group", "key": "grid-@1", "children": [
    ["role": "label", "name": "[[RUNE_IOS_"], ["role": "label", "name": "INPUT_OK]]"]
]]
let wrappedSaved = try publish(wrapped)
precondition(wrappedSaved.count == 2, "soft-wrapped marker remains valid")
enum WriteFailure: Error { case tree }
var names: [String] = []
do {
    _ = try QualificationEvidence.publish(report: report, tree: tree(witness), session: session, marker: marker,
        didPresent: true, write: { _, name in
            names.append(name)
            throw WriteFailure.tree
        })
    preconditionFailure("write failure must propagate")
} catch WriteFailure.tree {}
precondition(names == ["rune-tree.json"], "failed tree write cannot publish readiness")

// Execute the actual policy used before QualificationProbe injects UIKit input.
// First connection expiry is terminal even if another connection later attaches.
var expired = QualificationProgress()
precondition(!expired.claimInput(when: false))
precondition(expired.claimFailure())
precondition(!expired.claimInput(when: true), "late recovery cannot inject the resumed witness")
expired.publish { preconditionFailure("late ready cannot overwrite the failed report") }
precondition(!expired.claimFailure(), "a later error cannot overwrite the first failure")
precondition(expired.finished && !expired.injected, "failure is latched")
var connected = QualificationProgress()
precondition(connected.claimInput(when: true))
precondition(!connected.claimInput(when: true), "a live connection injects only once")
connected.publish { false }
precondition(!connected.finished, "waiting for the rendered tree is not failure")
connected.publish { true }
precondition(connected.finished)
precondition(connected.claimFailure(), "a post-success lifecycle failure must still be reported")
connected.publish { preconditionFailure("lifecycle failure cannot become success") }

// A reconnect can leave the witness visible after the live session disappears.
var reconnecting = QualificationProgress()
precondition(reconnecting.claimInput(when: true))
var reconnectWrites: [String] = []
for (observation, live) in [
    (report.merging(["session": NSNull()]) { _, new in new }, false),
    (report.merging(["session": "another-session"]) { _, new in new }, false),
    (report.merging(["connected": false]) { _, new in new }, false),
    (report, true),
    (report, true)
] {
    try reconnecting.publish {
        try QualificationEvidence.publish(report: observation, tree: tree(witness), session: session, marker: marker,
            didPresent: true, write: { _, name in reconnectWrites.append(name) })
    }
    precondition(reconnecting.finished == live, "only a live observation of the requested session can finish")
    precondition(!reconnecting.claimInput(when: live), "reconnect cannot reinject the witness")
    precondition(reconnectWrites == (live ? ["rune-tree.json", "rune-qualification.json"] : []))
}

// The failure report acknowledges seal cleanup, including a failed seal.
// Neither callback fabricates frame counts or a rendered-input success.
var preparation: [String] = []
QualificationEvidence.preparationFailure(
    seal: { preparation.append("seal-returned") },
    publish: {
        precondition(preparation == ["seal-returned"])
        preparation.append("original-failure-published")
    })
precondition(preparation == ["seal-returned", "original-failure-published"])
""")
            compiled = subprocess.run(["/usr/bin/xcrun", "--sdk", "macosx", "swiftc", "-swift-version", "6",
                            "-warnings-as-errors", str(source), str(evidence), str(main), "-o", str(root / "cadence")],
                           env=build.apple_environment(root), capture_output=True, text=True, timeout=120)
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            subprocess.run([str(root / "cadence")], check=True, timeout=10)


if __name__ == "__main__":
    unittest.main()
