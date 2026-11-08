import UIKit

// UIKit implements UITextInput, including marked text and dictation. Rune
// paints the document; this backing view supplies the OS input services.
// Raw-capture mutation ordering follows phux-mobile's RawTerminalTextField.
@MainActor
final class RuneInput: UITextView, UITextViewDelegate {
    static let sentinel = "\u{200B}"
    private(set) var context: InputContext?
    var send: (([String: Any], InputContext) -> [String: Any]?)?
    var didDismiss: ((InputContext) -> Void)?
    private var mutationDepth = 0
    private var synchronizing = false
    private var composition: InputContext?
    private var dismissedToken: UInt64?
    private var selection = InputSelection(anchor: 0, focus: 0)
    private var raw: Bool { context?.kind != "field" }
    private var dictating: Bool { textInputMode?.primaryLanguage == "dictation" }
    private var composing: Bool { markedTextRange != nil || dictating }

    init() {
        super.init(frame: .zero, textContainer: nil)
        delegate = self
        backgroundColor = .clear; textColor = .clear; tintColor = .clear
        isScrollEnabled = false; contentInset = .zero; textContainerInset = .zero
        keyboardType = .default; autocapitalizationType = .none
        autocorrectionType = .no; spellCheckingType = .no
        smartDashesType = .no; smartQuotesType = .no; smartInsertDeleteType = .no
        accessibilityLabel = "Terminal keyboard"
        accessibilityIdentifier = "rune-native-input"
        NotificationCenter.default.addObserver(self, selector: #selector(publish),
            name: UITextInputMode.currentInputModeDidChangeNotification, object: nil)
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    func synchronize(_ next: InputContext) {
        synchronizing = true
        defer {
            synchronizing = false
            flushFinishedComposition()
        }
        if composing, composition == nil { composition = context }
        let next = cancelStaleComposition(next)
        if context?.token != next.token { retire() }
        guard next.wanted, dismissedToken != next.token else {
            retire()
            return
        }
        dismissedToken = nil
        context = next
        accessibilityLabel = next.kind == "field" ? "Edit field" : "Terminal keyboard"
        if composition == nil { restoreDocument(next) }
        if !isFirstResponder { becomeFirstResponder() }
    }

    private func sameDocument(_ first: InputContext, _ second: InputContext) -> Bool {
        first.token == second.token && first.revision == second.revision
    }

    private func flushFinishedComposition() {
        // A frame can arrive after dictation mode ends but before its mode-change
        // notification. Publish the buffered result, rather than overwrite it.
        guard composition != nil, !composing else { return }
        publish()
    }

    private func cancelStaleComposition(_ next: InputContext) -> InputContext {
        guard let composition, !sameDocument(composition, next) else { return next }
        retire() // unmark/resign callbacks must not commit the previous document
        guard composition.token == next.token else { return next }
        // The old revision cannot address the current owner. Cancel only its
        // preedit overlay using the freshly received authoritative context.
        let result = send?(["op":"cancel"], next)
        return InputContext(result?["input"]) ?? next
    }

    private func restoreDocument(_ next: InputContext) {
        let desired = next.kind == "field" ? next.text : Self.sentinel
        selection = next.kind == "field"
            ? InputSelection(anchor: next.anchor, focus: next.focus)
            : InputSelection(anchor: 1, focus: 1)
        programmatic {
            if text != desired { text = desired }
            if selectedRange != selection.range { selectedRange = selection.range }
        }
    }

    func retire() {
        context = nil
        composition = nil
        programmatic {
            resignFirstResponder() // cancel dictation while callbacks are disarmed
            super.unmarkText()
            text = ""
        }
    }

    private func programmatic(_ body: () -> Void) {
        mutationDepth += 1
        defer { mutationDepth -= 1 }
        body()
    }
    private func mutate(_ body: () -> Void) {
        guard context != nil else { return }
        if dictating, composition == nil { composition = context }
        programmatic(body)
        publish()
    }
    override func insertText(_ value: String) {
        if value == "\n", context?.multiline != true, !composing {
            named("Enter")
        } else { mutate { super.insertText(value) } }
    }
    override func deleteBackward() { mutate { super.deleteBackward() } }
    override func replace(_ range: UITextRange, withText value: String) {
        mutate { super.replace(range, withText: value) }
    }
    override func setMarkedText(_ value: String?, selectedRange: NSRange) {
        if composition == nil { composition = context }
        mutate { super.setMarkedText(value, selectedRange: selectedRange) }
    }
    override func unmarkText() { mutate { super.unmarkText() } }

    @objc private func publish() {
        guard mutationDepth == 0, !synchronizing, let current = context else { return }
        if composing, composition == nil { composition = current }
        let captured = composition ?? current
        guard sameDocument(captured, current) else { return }
        if let marked = markedTextRange {
            let value = text(in: marked) ?? ""
            let selection = selectedTextRange
            let start = selection.map { max(0, offset(from: marked.start, to: $0.start)) } ?? 0
            let end = selection.map { max(0, offset(from: marked.start, to: $0.end)) } ?? start
            emit(["op":"preedit", "text":value, "anchor":start, "focus":end], captured)
            return
        }
        if dictating { return }
        let finishedComposition = composition != nil
        composition = nil
        if raw { publishRaw(captured, finishedComposition: finishedComposition) } else {
            selection.update(selectedRange)
            emit(["op":"replace", "expected":captured.text, "text":text ?? "",
                  "anchor":selection.anchor, "focus":selection.focus], captured)
        }
    }

    private func publishRaw(_ captured: InputContext, finishedComposition: Bool) {
        let value = text ?? ""
        if value == Self.sentinel, !finishedComposition { return }
        let hasSentinel = value.hasPrefix(Self.sentinel)
        let committed = hasSentinel ? String(value.dropFirst()) : value
        // Reset before the next key or a reentrant callback can arrive.
        programmatic { text = Self.sentinel; selectedRange = NSRange(location: 1, length: 0) }
        if finishedComposition, committed.isEmpty {
            emit(["op":"cancel"], captured)
            return
        }
        // An IME/dictation replacement can remove the sentinel itself. Only an
        // empty document outside a composition means an actual Backspace.
        if value.isEmpty { emit(["op":"key", "name":"Backspace"], captured) }
        if !committed.isEmpty { emit(["op":"text", "text":committed], captured) }
    }

    private func emit(_ operation: [String: Any], _ captured: InputContext) {
        guard let result = send?(operation, captured) else { return }
        if let next = InputContext(result["input"]) {
            // Rejected edits (including recoverable C ABI errors) still carry
            // an authoritative document. Never leave a stale UIKit buffer.
            if result["accepted"] as? Bool != true { retire() }
            synchronize(next)
        }
        if let text = result["copied_text"] as? String {
            UIPasteboard.general.string = text
        }
    }
    func named(_ name: String, ctrl: Bool = false, alt: Bool = false,
               shift: Bool = false, command: Bool = false) {
        guard !composing, let captured = context else { return }
        emit(["op":"key", "name":name, "ctrl":ctrl, "alt":alt,
              "shift":shift, "command":command], captured)
    }
    override func paste(_ sender: Any?) {
        guard raw else { super.paste(sender); return }
        guard !composing else { return }
        if let captured = context, let value = UIPasteboard.general.string {
            emit(["op":"paste", "text":value], captured)
        }
    }
    override func copy(_ sender: Any?) {
        guard raw else { super.copy(sender); return }
        guard !composing else { return }
        if let captured = context { emit(["op":"copy"], captured) }
    }
    override func selectAll(_ sender: Any?) {
        guard raw else { super.selectAll(sender); return }
        guard !composing else { return }
        if let captured = context { emit(["op":"select_all"], captured) }
    }
    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let unhandled = Set(presses.filter { press in
            guard !composing, let key = press.key else { return true }
            return !handleKey(key.charactersIgnoringModifiers, flags: key.modifierFlags)
        })
        if !unhandled.isEmpty { super.pressesBegan(unhandled,with:event) }
    }

    // Kept separate from UIPress construction so the app's input contract
    // probe can exercise the same hardware routing as UIKit.
    @discardableResult
    func handleKey(_ characters: String, flags: UIKeyModifierFlags) -> Bool {
        guard !composing, context != nil else { return false }
        if raw, clipboardCommand(characters, flags: flags) { return true }
        let names = [UIKeyCommand.inputUpArrow:"Up", UIKeyCommand.inputDownArrow:"Down",
            UIKeyCommand.inputLeftArrow:"Left", UIKeyCommand.inputRightArrow:"Right",
            UIKeyCommand.inputEscape:"Escape", "\r":"Enter", "\t":"Tab", "\u{7F}":"Backspace"]
        let name = names[characters]
        guard routesKey(name, flags: flags) else { return false }
        named(name ?? characters, ctrl:flags.contains(.control),
              alt:flags.contains(.alternate), shift:flags.contains(.shift), command:flags.contains(.command))
        return true
    }

    private func routesKey(_ name: String?, flags: UIKeyModifierFlags) -> Bool {
        if !raw {
            if name == "Enter" { return context?.multiline != true }
            if name == "Up" || name == "Down" { return context?.multiline != true }
            return name == "Escape" || name == "Tab"
        }
        // Option-only characters/dead keys stay UIKit-owned unless the
        // person explicitly chose Option as Meta in terminal Settings.
        if name != nil { return true }
        return flags.contains(.control) || flags.contains(.command)
            || (flags.contains(.alternate) && context?.optionAsMeta == true)
    }

    private func clipboardCommand(_ characters: String, flags: UIKeyModifierFlags) -> Bool {
        guard flags.intersection([.command, .control, .alternate]) == .command else { return false }
        switch characters.lowercased() {
        case "c": copy(nil)
        case "v": paste(nil)
        case "a": selectAll(nil)
        default: return false
        }
        return true
    }
    func textViewDidChange(_ textView: UITextView) { publish() }
    func textViewDidChangeSelection(_ textView: UITextView) {
        guard mutationDepth == 0, !synchronizing, markedTextRange == nil,
              !dictating, composition == nil, let captured = context else { return }
        if raw {
            programmatic { selectedRange = NSRange(location: (text as NSString).length, length: 0) }
        } else {
            selection.update(selectedRange)
            emit(["op":"selection", "anchor":selection.anchor,
                  "focus":selection.focus], captured)
        }
    }
    func textViewDidEndEditing(_ textView: UITextView) {
        guard !synchronizing, let captured = context else { return }
        dismissedToken = captured.token
        context = nil
        composition = nil
        didDismiss?(captured)
    }
    override var accessibilityValue: String? {
        get { raw ? (text ?? "").replacingOccurrences(of: Self.sentinel, with: "") : text }
        set {}
    }
}
