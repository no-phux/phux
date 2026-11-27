import AppIntents
import AVFAudio
import Speech
import UIKit

/// The Action Button's shortcut (or Siri's, or a Shortcuts tile): open
/// Phux listening, and send what was said to one session. Which session is
/// the shortcut's own setting, `commander` until the user names another.
struct DictateIntent: AppIntent {
    static let title: LocalizedStringResource = "Dictate to Phux"
    static let description = IntentDescription(
        "Opens Phux listening, then sends what you say to a session's terminal.")
    static let supportedModes: IntentModes = .foreground

    @Parameter(title: "Session", default: "commander")
    var session: String

    static var parameterSummary: some ParameterSummary { Summary("Dictate to \(\.$session)") }

    @MainActor
    func perform() async throws -> some IntentResult {
        DictationCenter.shared.request(session: session)
        return .result()
    }
}

struct PhuxShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: DictateIntent(), phrases: ["Dictate to \(.applicationName)"],
                    shortTitle: "Dictate", systemImageName: "mic.fill")
    }
}

/// Where a run of the shortcut waits for the app to be able to listen: a
/// cold launch runs it before the window exists.
@MainActor
final class DictationCenter {
    static let shared = DictationCenter()
    private var pending: String?
    /// Shows the dictation sheet for a session; set once the window can.
    var present: ((String) -> Void)? {
        didSet { if let present, let session = pending { pending = nil; present(session) } }
    }

    func request(session: String) {
        let session = session.trimmingCharacters(in: .whitespacesAndNewlines)
        let name = session.isEmpty ? "commander" : session
        if let present { present(name) } else { pending = name }
    }
}

/// A sheet that listens and shows what it heard, editable, over the
/// terminal; Send hands the words to Rune for their session. On-device
/// recognition where the phone has it, so the words stay on the phone.
@MainActor
final class DictationSheet: UIViewController {
    let session: String
    var send: ((String) -> Void)?
    private let words = UITextView()
    private let status = UILabel()
    private let sendButton = UIButton(configuration: .prominentGlass())
    private let engine = AVAudioEngine()
    private var task: SFSpeechRecognitionTask?
    private var request: SFSpeechAudioBufferRecognitionRequest?
    /// The user has edited by hand: the recognizer no longer writes.
    private var edited = false
    /// Listening has begun, or been refused; it never begins twice.
    private var began = false
    /// Sent, cancelled or replaced: a permission answered late starts nothing.
    private var closed = false
    private var tapped = false

    init(session: String) {
        self.session = session
        super.init(nibName: nil, bundle: nil)
        modalPresentationStyle = .pageSheet
        if let sheet = sheetPresentationController {
            sheet.detents = [.medium(), .large()]
            sheet.prefersGrabberVisible = true
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func viewDidLoad() {
        super.viewDidLoad()
        let title = UILabel()
        title.text = session
        title.font = .preferredFont(forTextStyle: .headline)
        title.adjustsFontForContentSizeCategory = true
        title.accessibilityLabel = "Dictating to \(session)"
        status.font = .preferredFont(forTextStyle: .subheadline)
        status.adjustsFontForContentSizeCategory = true
        status.textColor = .secondaryLabel
        status.text = "Listening…"
        words.font = .preferredFont(forTextStyle: .title3)
        words.adjustsFontForContentSizeCategory = true
        words.backgroundColor = .clear
        words.accessibilityLabel = "What you said"
        words.delegate = self

        var cancel = UIButton.Configuration.glass()
        cancel.image = UIImage(systemName: "xmark")
        let cancelButton = UIButton(configuration: cancel, primaryAction: UIAction { [weak self] _ in
            self?.finish(sending: false)
        })
        cancelButton.accessibilityLabel = "Cancel"
        sendButton.configuration?.title = "Send"
        sendButton.configuration?.image = UIImage(systemName: "arrow.up")
        sendButton.configuration?.imagePlacement = .trailing
        sendButton.configuration?.imagePadding = 6
        sendButton.addAction(UIAction { [weak self] _ in self?.finish(sending: true) }, for: .primaryActionTriggered)
        sendButton.isEnabled = false

        let header = UIStackView(arrangedSubviews: [cancelButton, title, sendButton])
        header.alignment = .center
        header.spacing = 12
        title.textAlignment = .center
        title.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let column = UIStackView(arrangedSubviews: [header, status, words])
        column.axis = .vertical
        column.spacing = 12
        column.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(column)
        let margins = view.layoutMarginsGuide
        NSLayoutConstraint.activate([
            column.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 20),
            column.leadingAnchor.constraint(equalTo: margins.leadingAnchor),
            column.trailingAnchor.constraint(equalTo: margins.trailingAnchor),
            column.bottomAnchor.constraint(equalTo: view.keyboardLayoutGuide.topAnchor, constant: -12),
        ])
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        Task { await listen() }
    }

    // Gone, not a swipe that bounced back: that keeps listening.
    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        closed = true
        stop()
    }

    /// Another press of the Action Button while listening: send.
    func pressedAgain() { finish(sending: true) }

    private func listen() async {
        // Once per sheet: a swipe that bounces back appears it again.
        guard !began else { return }
        began = true
        let speech = await Self.speechAuthorization()
        guard !closed else { return }
        guard speech == .authorized else {
            return refuse("Speech recognition is off for Phux. Turn it on in Settings, or type here.")
        }
        let microphone = await AVAudioApplication.requestRecordPermission()
        guard !closed else { return }
        guard microphone else {
            return refuse("The microphone is off for Phux. Turn it on in Settings, or type here.")
        }
        guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable else {
            return refuse("Dictation isn't available right now. Type here instead.")
        }
        do {
            try start(recognizer)
        } catch {
            stop()
            refuse("Couldn't listen: \(error.localizedDescription)")
        }
    }

    /// Built outside the main actor: Speech answers on its own queue.
    nonisolated private static func speechAuthorization() async -> SFSpeechRecognizerAuthorizationStatus {
        await withCheckedContinuation { done in
            SFSpeechRecognizer.requestAuthorization { done.resume(returning: $0) }
        }
    }

    private func start(_ recognizer: SFSpeechRecognizer) throws {
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        request.requiresOnDeviceRecognition = recognizer.supportsOnDeviceRecognition
        // Set first, so a failure below is still undone by stop().
        self.request = request
        let audio = AVAudioSession.sharedInstance()
        try audio.setCategory(.record, mode: .measurement, options: .duckOthers)
        try audio.setActive(true, options: .notifyOthersOnDeactivation)
        let input = engine.inputNode
        input.installTap(onBus: 0, bufferSize: 1024, format: input.outputFormat(forBus: 0),
                         block: Self.feed(request))
        tapped = true
        engine.prepare()
        try engine.start()
        task = recognizer.recognitionTask(with: request, resultHandler: Self.heard(self))
    }

    /// Built outside the main actor: the tap runs on the audio thread.
    nonisolated private static func feed(_ request: SFSpeechAudioBufferRecognitionRequest) -> AVAudioNodeTapBlock {
        { buffer, _ in request.append(buffer) }
    }

    /// Built outside the main actor: results arrive on the recognizer's queue.
    nonisolated private static func heard(_ sheet: DictationSheet)
        -> @Sendable (SFSpeechRecognitionResult?, (any Error)?) -> Void {
        { [weak sheet] result, error in
            let text = result?.bestTranscription.formattedString
            let final = result?.isFinal ?? false
            let failed = error != nil
            DispatchQueue.main.async { sheet?.show(text, final: final, failed: failed) }
        }
    }

    private func show(_ text: String?, final: Bool, failed: Bool) {
        if let text, !edited {
            words.text = text
            sendButton.isEnabled = !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
        if final || failed {
            stop()
            status.text = words.text.isEmpty ? "Didn't catch that. Type here instead." : "Done listening."
        }
    }

    private func refuse(_ message: String) {
        status.text = message
        words.becomeFirstResponder()
    }

    private func stop() {
        guard request != nil else { return }
        engine.stop()
        if tapped { engine.inputNode.removeTap(onBus: 0); tapped = false }
        request?.endAudio()
        request = nil
        task?.finish()
        task = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    /// Put away unsent, as Cancel does: another session's sheet replaces it.
    func cancel(then next: @escaping () -> Void) {
        closed = true
        stop()
        dismiss(animated: false, completion: next)
    }

    private func finish(sending: Bool) {
        closed = true
        stop()
        let text = words.text.trimmingCharacters(in: .whitespacesAndNewlines)
        if sending, !text.isEmpty { send?(text) }
        dismiss(animated: true)
    }
}

extension DictationSheet: UITextViewDelegate {
    func textViewDidBeginEditing(_ textView: UITextView) {
        // A hand edit wins over the recognizer from here on.
        let listening = request != nil
        edited = true
        stop()
        if listening { status.text = "Editing." }
    }

    func textViewDidChange(_ textView: UITextView) {
        sendButton.isEnabled = !textView.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}
