import UIKit

#if targetEnvironment(simulator)

// App-internal qualification probe: no XCTest dependency in the shipped host.
@MainActor
enum InputContractTests {
    private struct Failure: Error, CustomStringConvertible {
        let description: String
    }

    private static func require(_ value: @autoclosure () -> Bool, _ message: String) throws {
        if !value() { throw Failure(description: message) }
    }

    @MainActor
    private final class Fixture {
        let input = RuneInput()
        var snapshot: [String: Any]
        var operations: [[String: Any]] = []
        var reject = false

        init(kind: String = "field", text: String = "abc", anchor: Int = 3,
             focus: Int = 3, multiline: Bool = false) throws {
            snapshot = ["token":UInt64(7), "revision":UInt64(0), "kind":kind,
                "text":text, "anchor":anchor, "focus":focus,
                "multiline":multiline, "wanted":true]
            input.send = { [weak self] operation, context in
                self?.reply(operation, context: context)
            }
            try synchronize()
        }

        func synchronize() throws {
            guard let context = InputContext(snapshot) else {
                throw Failure(description:"Input fixture produced an invalid snapshot.")
            }
            input.synchronize(context)
        }

        private func reply(_ operation: [String: Any], context: InputContext) -> [String: Any] {
            operations.append(operation.merging(["revision":context.revision]) { _, new in new })
            if reject { return ["accepted":false, "input":snapshot, "error":"Rejected edit"] }
            if operation["op"] as? String == "replace" {
                snapshot["text"] = operation["text"]
                snapshot["anchor"] = operation["anchor"]
                snapshot["focus"] = operation["focus"]
                snapshot["revision"] = context.revision + 1
            }
            if operation["op"] as? String == "selection" {
                snapshot["anchor"] = operation["anchor"]
                snapshot["focus"] = operation["focus"]
            }
            return ["accepted":true, "input":snapshot]
        }
    }

    static func run() throws -> [String] {
        try fieldEditing()
        try rejectedEdit()
        try compositionRevision()
        try rawComposition()
        try rawReplacement()
        try hardwareRouting()
        try directionalSelection()
        try dismissal()
        return ["field-editing", "rejected-edit", "composition-revision", "raw-composition",
                "raw-replacement", "hardware-routing", "directional-selection", "dismissal"]
    }

    private static func fieldEditing() throws {
        let f = try Fixture()
        try require(!f.input.handleKey("\u{7F}", flags: []), "UIKit must own field Backspace")
        f.input.deleteBackward()
        f.input.insertText("x")
        try require(f.input.text == "abx", "Backspace must not resurrect text")
        try require(f.snapshot["text"] as? String == "abx", "Field reply must reconcile immediately")
        let multiline = try Fixture(multiline: true)
        try require(!multiline.input.handleKey("\r", flags: []), "UIKit must own multiline Return")
        multiline.input.insertText("\n")
        try require(multiline.input.text == "abc\n", "Multiline Return must insert newline")
        let single = try Fixture()
        try require(single.input.handleKey("\r", flags: []), "Single-line Return must submit")
        try require(single.operations.last?["name"] as? String == "Enter", "Single-line named submit missing")
        single.snapshot["text"] = "updated"
        single.snapshot["anchor"] = 7; single.snapshot["focus"] = 7
        single.snapshot["revision"] = UInt64(1)
        single.input.named("Tab")
        try require(single.input.text == "updated", "Accepted named key must reconcile authoritative field text")
    }

    private static func rejectedEdit() throws {
        let f = try Fixture()
        f.reject = true
        f.input.insertText("lost")
        try require(f.input.text == "abc", "Rejected edit must restore authoritative text")
        try require(f.input.context?.text == "abc", "Recoverable error must keep host armed")
        f.reject = false
        f.input.insertText("x")
        try require(f.input.text == "abcx", "Next edit must start from recovered snapshot")
    }

    private static func compositionRevision() throws {
        let f = try Fixture()
        f.input.setMarkedText("に", selectedRange: NSRange(location: 1, length: 0))
        try require(f.input.markedTextRange != nil, "Probe failed to start UIKit composition")
        try f.synchronize()
        try require(f.input.markedTextRange != nil, "Same revision must preserve marked text")
        f.snapshot["revision"] = UInt64(1)
        f.snapshot["text"] = "remote"
        f.snapshot["anchor"] = 6; f.snapshot["focus"] = 6
        try f.synchronize()
        f.input.unmarkText()
        try require(f.input.text == "remote", "External revision must cancel stale composition")
        try require(f.operations.contains { $0["op"] as? String == "cancel" }, "External revision left a stale preedit overlay")
        let edits = f.operations.filter { $0["op"] as? String == "replace" }
        try require(edits.allSatisfy { $0["text"] as? String == "remote" }, "Stale composition document was committed")
    }

    private static func rawComposition() throws {
        let f = try Fixture(kind: "terminal", text: "", anchor: 0, focus: 0)
        f.input.setMarkedText("に", selectedRange: NSRange(location: 1, length: 0))
        f.input.setMarkedText("日本", selectedRange: NSRange(location: 2, length: 0))
        try require(!f.input.handleKey("c", flags: .command), "Composition must own hardware commands")
        f.input.copy(nil)
        f.input.unmarkText()
        f.input.textViewDidChange(f.input)
        let committed = f.operations.filter { $0["op"] as? String == "text" }
        try require(committed.count == 1, "CJK commit must be sent exactly once")
        try require(committed.first?["text"] as? String == "日本", "CJK commit lost characters")
        try require(!f.operations.contains { $0["op"] as? String == "copy" }, "Copy escaped composition guard")
        try require(f.input.text == RuneInput.sentinel, "Raw buffer was not reset")
    }

    private static func rawReplacement() throws {
        let f = try Fixture(kind: "terminal", text: "", anchor: 0, focus: 0)
        let range = f.input.textRange(from: f.input.beginningOfDocument, to: f.input.endOfDocument)!
        f.input.replace(range, withText: "你好")
        try require(f.operations.count == 1, "Sentinel replacement must not synthesize Backspace")
        try require(f.operations.first?["text"] as? String == "你好", "Replacement dropped committed text")
        f.input.deleteBackward()
        try require(f.operations.last?["name"] as? String == "Backspace", "Actual sentinel deletion must send Backspace")
        // Dictation's fallback delivery uses insertText; a whole phrase must
        // survive sentinel reset and a trailing delegate callback exactly once.
        f.input.insertText("spoken 日本 🧑‍💻")
        f.input.textViewDidChange(f.input)
        let phrases = f.operations.filter { $0["text"] as? String == "spoken 日本 🧑‍💻" }
        try require(phrases.count == 1, "Dictation-style phrase delivery was dropped or duplicated")
    }

    private static func hardwareRouting() throws {
        let f = try Fixture(kind: "terminal", text: "", anchor: 0, focus: 0)
        try require(!f.input.handleKey("e", flags: .alternate), "Option-only text must stay with UIKit")
        try require(f.input.handleKey("c", flags: .command), "Command-C must use explicit copy")
        try require(f.operations.last?["op"] as? String == "copy", "Command-C routed to desktop clipboard")
        try require(f.input.handleKey("a", flags: .command), "Command-A must use explicit select-all")
        try require(f.operations.last?["op"] as? String == "select_all", "Command-A routed to desktop handler")
        // Keep the person's pasteboard untouched; direct paste routing is covered
        // by the qualification probe when it installs a disposable clipboard.
    }

    private static func directionalSelection() throws {
        let f = try Fixture(anchor: 3, focus: 1)
        f.input.textViewDidChangeSelection(f.input)
        try require(f.operations.last?["anchor"] as? Int == 3, "Reversed anchor was normalized")
        try require(f.operations.last?["focus"] as? Int == 1, "Reversed focus was normalized")
        f.input.insertText("X")
        try require(f.input.text == "aX", "Directional selection replacement failed")
        var selection = InputSelection(anchor: 4, focus: 2)
        selection.update(NSRange(location: 1, length: 3))
        try require(selection.anchor == 4 && selection.focus == 1, "Moving reverse focus lost direction")
    }

    private static func dismissal() throws {
        let f = try Fixture()
        var dismissals = 0
        f.input.didDismiss = { _ in dismissals += 1 }
        f.input.textViewDidEndEditing(f.input)
        try f.synchronize()
        try require(f.input.context == nil, "Stale frame reopened natively dismissed input")
        try require(dismissals == 1, "Native dismissal must be emitted once")
        f.snapshot["token"] = UInt64(8)
        try f.synchronize()
        try require(f.input.context?.token == 8, "A new owner must be able to request input")
    }
}
#endif
