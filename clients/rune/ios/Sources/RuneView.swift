import UIKit
import QuartzCore

@MainActor
private final class DisplayTick: NSObject {
    weak var owner: RuneView?
    @objc func tick(_ link: CADisplayLink) { owner?.drawFrame() }
}

@MainActor
final class RuneView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    let editor = RuneInput()
    var connectionRequested: (() -> Void)?
    var groundChanged: ((UIColor) -> Void)?
    var failed: ((String) -> Void)?
    var qualification: QualificationProbe?
    private var configuration: [String: Any]
    private var bridge: NativeBridge?
    private let tick = DisplayTick()
    private var displayLink: CADisplayLink?
    private var timer: DispatchWorkItem?
    private var active = false
    private var drawing = false
    private var identities: [ObjectIdentifier: UInt64] = [:]
    private var nextIdentity: UInt64 = 0
    private var drawableRetries = 0
    isolated deinit { close() }

    init(configuration: [String: Any]) {
        self.configuration = configuration
        super.init(frame: .zero)
        active = UIApplication.shared.applicationState == .active
        isMultipleTouchEnabled = true
        // While the keyboard animates the bounds, keep the last frame at its
        // own size against the top edge rather than stretching it. Core
        // Animation names this edge for an unflipped layer: UIKit's top.
        // Unscaled contents need the layer's scale (set with each layout),
        // or a 3x drawable would show three times too large.
        layer.contentsGravity = .bottomLeft
        layer.masksToBounds = true
        addGestureRecognizer(UIPinchGestureRecognizer(target: self, action: #selector(pinched)))
        backgroundColor = UIColor(named: "LaunchGround") ?? .black
        editor.send = { [weak self] operation, context in self?.send(operation, context: context) }
        editor.didDismiss = { [weak self] context in _ = self?.send(["op":"dismiss"], context: context) }
        addSubview(editor)
        registerForTraitChanges([UITraitUserInterfaceStyle.self]) { (view: RuneView, _:UITraitCollection) in view.refresh() }
        NotificationCenter.default.addObserver(self,selector:#selector(preferencesChanged),
            name:UIAccessibility.reduceMotionStatusDidChangeNotification,object:nil)
        tick.owner = self
        let link = CADisplayLink(target: tick, selector: #selector(DisplayTick.tick))
        link.preferredFrameRateRange = CAFrameRateRange(minimum: 30, maximum: 120, preferred: 120)
        link.add(to: .main, forMode: .common)
        link.isPaused = true
        displayLink = link
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard active, window != nil, bounds.width > 0, bounds.height > 0 else { return }
        layer.contentsScale = window?.screen.scale ?? traitCollection.displayScale
        do {
            if bridge == nil {
                let box = WakeBox(); box.view = self
                bridge = try NativeBridge(layer: layer as! CAMetalLayer, size: bounds.size,
                    scale: window?.screen.scale ?? 1, dark: traitCollection.userInterfaceStyle != .light,
                    configuration: configuration, wake: box)
                configuration.removeValue(forKey: "token")
            }
            bridge?.resize(bounds.size, scale: window?.screen.scale ?? 1)
            bridge?.traits(dark: traitCollection.userInterfaceStyle != .light,
                           reducedMotion: UIAccessibility.isReduceMotionEnabled)
            drawFrame()
        } catch { fail(error) }
    }

    /// Whether a point of the view shows a point of the drawable: unscaled
    /// contents with the wrong layer scale draw the terminal several times
    /// too large, which no report from inside Rune can see.
    private var showsPixelsAtScale: Bool {
        guard let metal = layer as? CAMetalLayer else { return false }
        let (drawable, scale) = (metal.drawableSize, metal.contentsScale)
        return abs(drawable.width - bounds.width * scale) <= 1 && abs(drawable.height - bounds.height * scale) <= 1
    }

    /// The display link draws the next frame; drawing here as well would
    /// add an off-vsync frame to every touch sample.
    func wake() {
        guard active else { return }
        displayLink?.isPaused = false
        if bridge == nil { setNeedsLayout() }
    }

    /// Size, appearance or accessibility changed: lay out, which tells Rune.
    private func refresh() {
        setNeedsLayout()
        wake()
    }

    @objc private func preferencesChanged() { refresh() }

    /// Text size by pinch, a step at a time so the terminal refits a few
    /// times over a gesture instead of on every frame of it.
    @objc private func pinched(_ pinch: UIPinchGestureRecognizer) {
        guard active, pinch.state == .changed, abs(pinch.scale - 1) > 0.12 else { return }
        bridge?.zoom(larger: pinch.scale > 1)
        pinch.scale = 1
        wake()
    }

    func drawFrame() {
        guard active, !drawing, let bridge else { return }
        drawing = true
        defer { drawing = false }
        do {
            let frame = try bridge.frame()
            if let input = InputContext(frame["input"]) {
                let caret = input.caret ?? CGRect(x: 4, y: max(0,bounds.height - 24), width: 1, height: 20)
                editor.frame = CGRect(x: max(0,min(caret.minX,bounds.width-1)),
                    y: max(0,min(caret.minY,bounds.height-1)), width: 1, height: max(1,caret.height))
                editor.synchronize(input)
            }
            timer?.cancel(); timer = nil
            displayLink?.isPaused = !(frame["animating"] as? Bool ?? false)
            if let after = frame["wake_after_secs"] as? Double, after.isFinite {
                let work = DispatchWorkItem { [weak self] in self?.wake() }
                timer = work
                DispatchQueue.main.asyncAfter(deadline: .now() + max(0.001, after), execute: work)
            }
            if let requests = frame["requests"] as? [String: Any] { serve(requests) }
            if let ground = frame["ground"] as? [Double], ground.count == 3 {
                groundChanged?(UIColor(red: ground[0], green: ground[1], blue: ground[2], alpha: 1))
            }
            if frame["drew"] as? Bool == true { drawableRetries = 0 }
            else if timer == nil, drawableRetries < 3 {
                drawableRetries += 1
                let retry = DispatchWorkItem { [weak self] in self?.wake() }
                timer = retry
                DispatchQueue.main.asyncAfter(deadline:.now() + 0.05, execute:retry)
            }
            try qualification?.observe(bridge: bridge, input: editor, surfaceAtScale: showsPixelsAtScale)
        } catch { fail(error) }
    }

    /// What the frame asked of the system: Rune reaches no pasteboard, link
    /// opener or haptics from inside a layer, so UIKit answers for it.
    private func serve(_ requests: [String: Any]) {
        if let text = requests["copied_text"] as? String { UIPasteboard.general.string = text }
        if requests["paste"] as? Bool == true, let text = UIPasteboard.general.string {
            bridge?.paste(text)
            wake()
        }
        for link in requests["open_urls"] as? [String] ?? [] {
            guard let url = URL(string: link), ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "")
            else { continue }
            UIApplication.shared.open(url)
        }
        switch requests["feedback"] as? String {
        case "hold": UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        case "copied": UINotificationFeedbackGenerator().notificationOccurred(.success)
        default: break
        }
        if requests["connection"] as? Bool == true { connectionRequested?() }
    }

    private func send(_ operation: [String: Any], context: InputContext) -> [String: Any]? {
        guard active, let bridge else { return nil }
        do { let result = try bridge.input(operation, context: context); wake(); return result }
        catch { fail(error); return nil }
    }

    func setActive(_ active: Bool, keepingKeyboard: Bool = false) {
        guard self.active != active else {
            // Already inactive with its keyboard kept; now it must go.
            if !active, !keepingKeyboard { editor.retire() }
            return
        }
        if keepingKeyboard { editor.suspend() } else { editor.retire() }
        if !active {
            for identity in identities.values { bridge?.touch(identity, phase:3, point:.zero, seconds:ProcessInfo.processInfo.systemUptime) }
            identities.removeAll()
            timer?.cancel(); timer = nil
            displayLink?.isPaused = true
        }
        bridge?.active(active)
        if let bridge { qualification?.active(active,bridge:bridge) }
        self.active = active
        if active { refresh() }
    }

    func close() {
        setActive(false)
        displayLink?.invalidate(); displayLink = nil
        editor.retire(); editor.resignFirstResponder()
        bridge?.close(); bridge = nil
    }

    private func fail(_ error: Error) {
        setActive(false)
        failed?(error.localizedDescription)
    }

    private func touches(_ touches: Set<UITouch>, phase: UInt32) {
        guard active else { return }
        for touch in touches {
            let key = ObjectIdentifier(touch)
            if phase == 0 { nextIdentity += 1; identities[key] = nextIdentity }
            guard let identity = identities[key] else { continue }
            bridge?.touch(identity, phase:phase, point:touch.location(in:self), seconds:touch.timestamp)
            if phase >= 2 { identities.removeValue(forKey:key) }
        }
        wake()
    }
    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) { self.touches(touches,phase:0) }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) { self.touches(touches,phase:1) }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) { self.touches(touches,phase:2) }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { self.touches(touches,phase:3) }
}
