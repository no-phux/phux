import UIKit
import QuartzCore
import Network

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
    private var cadence = FrameCadence()
    private var controls: NativeControls?
    private let attachments = Attachments()
    private let heldMenu = HeldMenu()
    private let path = NWPathMonitor()
    /// A tapped card's link, until Rune can take it.
    private var link: String?
    /// Rune's link to its computer is up, as of the last drawn frame.
    private(set) var attached = false
    private var pathSettled = false
    private var pathWork: DispatchWorkItem?
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
        attachments.covering = { [weak self] covered in
            self?.editor.covered = covered
            if !covered { self?.wake() }
        }
        heldMenu.picked = { [weak self] request, id in
            _ = self?.bridge?.menuPick(request, id)
            self?.wake()
        }
        heldMenu.dismissed = { [weak self] request in
            self?.bridge?.menuDismiss(request)
            self?.wake()
        }
        attachments.picked = { [weak self] request, file in
            self?.bridge?.attach(file, request: request)
            self?.wake()
        }
        // The text size and contrast are the view's own traits; the
        // appearance is the scene's (see `systemDark`).
        registerForTraitChanges([UITraitPreferredContentSizeCategory.self, UITraitAccessibilityContrast.self]) {
            (view: RuneView, _:UITraitCollection) in view.refresh()
        }
        for name in [UIAccessibility.reduceMotionStatusDidChangeNotification,
                     UIAccessibility.reduceTransparencyStatusDidChangeNotification] {
            NotificationCenter.default.addObserver(self,selector:#selector(preferencesChanged),name:name,object:nil)
        }
        tick.owner = self
        let link = CADisplayLink(target: tick, selector: #selector(DisplayTick.tick))
        link.preferredFrameRateRange = CAFrameRateRange(minimum: 30, maximum: 120, preferred: 120)
        link.add(to: .main, forMode: .common)
        link.isPaused = true
        displayLink = link
        let box = PathBox(); box.view = self
        path.pathUpdateHandler = { _ in box.schedule() }
        path.start(queue: DispatchQueue(label: "sh.phux.rune.path"))
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard active, window != nil, bounds.width > 0, bounds.height > 0 else { return }
        layer.contentsScale = window?.screen.scale ?? traitCollection.displayScale
        // Acquisition must see this flag: wgpu then presents with the same
        // Core Animation transaction as UIKit's keyboard/bounds change.
        let metal = layer as! CAMetalLayer
        let transactional = metal.presentsWithTransaction
        metal.presentsWithTransaction = true
        defer { metal.presentsWithTransaction = transactional }
        do {
            if bridge == nil {
                let box = WakeBox(); box.view = self
                bridge = try NativeBridge(layer: layer as! CAMetalLayer, size: bounds.size,
                    scale: window?.screen.scale ?? 1, dark: systemDark,
                    configuration: configuration, wake: box)
                configuration.removeValue(forKey: "token")
                if let gateway = LivePush.gateway {
                    bridge?.setLive(device: LivePush.device, gateway: gateway + "/live", grant: LivePush.grant)
                }
            }
            bridge?.resize(bounds.size, scale: window?.screen.scale ?? 1)
            bridge?.traits(dark: systemDark, reducedMotion: UIAccessibility.isReduceMotionEnabled,
                           increasedContrast: traitCollection.accessibilityContrast == .high,
                           reduceTransparency: UIAccessibility.isReduceTransparencyEnabled,
                           contentSize: traitCollection.preferredContentSizeCategory)
            drawFrame()
        } catch { fail(error) }
    }

    /// The system's appearance. The window takes the terminal theme's mode
    /// (`RuneController.paint`), so the view's own style is the theme's:
    /// the scene's is the user's setting, which themes that follow the
    /// system read.
    private var systemDark: Bool {
        (window?.windowScene?.traitCollection ?? traitCollection).userInterfaceStyle != .light
    }

    private weak var observedScene: UIWindowScene?
    private var sceneObservation: (any UITraitChangeRegistration)?
    override func didMoveToWindow() {
        super.didMoveToWindow()
        let scene = window?.windowScene
        guard scene !== observedScene else { return }
        if let old = sceneObservation { observedScene?.unregisterForTraitChanges(old) }
        observedScene = scene
        sceneObservation = scene?.registerForTraitChanges([UITraitUserInterfaceStyle.self]) {
            [weak self] (_: UIWindowScene, _: UITraitCollection) in self?.refresh()
        }
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
        // A stream of input/network wakes must not bypass a failed drawable's
        // cooldown. The already-scheduled retry still draws the latest state.
        guard cadence.acceptsWake(timerScheduled: timer != nil) else { return }
        resumeDrawing()
    }

    private func resumeDrawing() {
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

    /// Wi-Fi to cellular, or the VPN coming up: tell the connection once the
    /// burst of updates settles. The first update only reports the path the
    /// connection was already dialed on.
    func pathChanged() {
        guard pathSettled else { pathSettled = true; return }
        pathWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.active else { return }
            self.bridge?.pathChanged()
            self.wake()
        }
        pathWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4, execute: work)
    }

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
        guard window != nil, !isHidden, bounds.width > 0, bounds.height > 0 else {
            timer?.cancel(); timer = nil
            displayLink?.isPaused = true
            return
        }
        drawing = true
        defer { drawing = false }
        do {
            // A cold tap from the card waits for the computer.
            if attached, let link { self.link = nil; _ = bridge.openLink(link) }
            let frame = try bridge.frame()
            synchronizeInput(frame)
            // Not on an undrawn frame: the controls stay where it last drew.
            if let specs = frame["controls"] as? [[String: Any]] { nativeControls().update(specs) }
            scheduleFrame(frame)
            if let requests = frame["requests"] as? [String: Any] { serve(requests) }
            updateGround(frame)
            try qualification?.observe(bridge: bridge, input: editor, surfaceAtScale: showsPixelsAtScale,
                                       didPresent: frame["drew"] as? Bool == true)
        } catch { fail(error) }
    }

    func openLink(_ link: String) {
        self.link = link
        wake()
    }

    private func synchronizeInput(_ frame: [String: Any]) {
        guard let input = InputContext(frame["input"]) else { return }
        let caret = input.caret ?? CGRect(x: 4, y: max(0,bounds.height - 24), width: 1, height: 20)
        editor.frame = CGRect(x: max(0,min(caret.minX,bounds.width-1)),
            y: max(0,min(caret.minY,bounds.height-1)), width: 1, height: max(1,caret.height))
        editor.synchronize(input)
    }

    private func scheduleFrame(_ frame: [String: Any]) {
        timer?.cancel(); timer = nil
        let drew = frame["drew"] as? Bool == true
        displayLink?.isPaused = !drew || !(frame["animating"] as? Bool ?? false)
        guard let after = cadence.nextWake(drew: drew, after: frame["wake_after_secs"] as? Double) else { return }
        let work = DispatchWorkItem { [weak self] in self?.resumeDrawing() }
        timer = work
        DispatchQueue.main.asyncAfter(deadline: .now() + after, execute: work)
    }

    private func updateGround(_ frame: [String: Any]) {
        guard frame["drew"] as? Bool == true,
              let ground = frame["ground"] as? [Double], ground.count == 3 else { return }
        let color = UIColor(red: ground[0], green: ground[1], blue: ground[2], alpha: 1)
        // Expanded, top-pinned contents expose this surface, not just the
        // controller behind it. Never expose the old launch colour as a band.
        if backgroundColor != color { backgroundColor = color }
        groundChanged?(color)
    }

    /// What the frame asked of the system: Rune reaches no pasteboard, link
    /// opener or haptics from inside a layer, so UIKit answers for it.
    private func serve(_ requests: [String: Any]) {
        if let text = requests["copied_text"] as? String { UIPasteboard.general.string = text }
        if requests["paste"] as? Bool == true { paste(requests["attaching"] as? UInt64) }
        if let at = requests["attach"] as? [Double], at.count == 4, let request = requests["attaching"] as? UInt64 {
            attachments.show(request: request, over: self, at: CGRect(x: at[0], y: at[1], width: at[2], height: at[3]))
        }
        heldMenu.reconcile(requests["menu_request"] as? UInt64)
        if let menu = requests["menu"] as? [String: Any] { heldMenu.show(menu, over: self) }
        for link in requests["open_urls"] as? [String] ?? [] {
            guard let url = URL(string: link), ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "")
            else { continue }
            UIApplication.shared.open(url)
        }
        switch requests["feedback"] as? String {
        case "hold": UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        case "copied": UINotificationFeedbackGenerator().notificationOccurred(.success)
        case "detent": UIImpactFeedbackGenerator(style: .light).impactOccurred()
        case "threshold": UIImpactFeedbackGenerator(style: .rigid).impactOccurred()
        default: break
        }
        attached = requests["attached"] as? Bool ?? false
        if qualification == nil { AgentCard.shared.update(requests["presence"]) }
        if requests["connection"] as? Bool == true { connectionRequested?() }
    }

    /// The pasteboard's text, or its image as an attachment to `request`.
    private func paste(_ request: UInt64?) {
        if let text = UIPasteboard.general.string { bridge?.paste(text) }
        else if let request, let image = Attachments.pasted() { bridge?.attach(image, request: request) }
        wake()
    }

    private func nativeControls() -> NativeControls {
        if let controls { return controls }
        let made = NativeControls(host: self)
        made.pressed = { [weak self] id in self?.press(id) }
        controls = made
        return made
    }

    /// Simulator qualification: the Liquid Glass controls on screen now.
    var glassEvidence: [[String: Any]] { controls?.evidence ?? [] }
    /// Simulator qualification: tap `id`'s real UIKit button.
    func tapNativeControl(_ id: String) -> Bool { controls?.tap(id) ?? false }

    /// A tap on a Liquid Glass control runs its Rune press in the next frame.
    private func press(_ id: String) {
        guard active, let bridge else { return }
        if bridge.press(id) { wake() }
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
        if active { cadence.reset(); refresh() }
    }

    func close() {
        setActive(false)
        displayLink?.invalidate(); displayLink = nil
        path.cancel(); pathWork?.cancel(); pathWork = nil
        editor.retire(); editor.resignFirstResponder()
        controls?.removeAll()
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
