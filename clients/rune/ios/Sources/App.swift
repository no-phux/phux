import UIKit

@main
@MainActor
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name:"Rune", sessionRole:session.role)
        configuration.delegateClass = SceneDelegate.self
        return configuration
    }
}

@objc(PhuxRuneSceneDelegate)
@MainActor
final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private var root: RuneController?
    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        phux_rune_ios_prewarm() // Compile the renderer while UIKit builds the window.
        let controller = RuneController()
        root = controller
        if let url = options.urlContexts.first?.url { controller.importPairing(url) }
        let window = UIWindow(windowScene:scene)
        // Rune draws its own header; a UIKit bar above it would double the chrome.
        window.rootViewController = controller
        self.window = window
        window.makeKeyAndVisible()
    }
    func scene(_ scene: UIScene, openURLContexts contexts: Set<UIOpenURLContext>) {
        if let url = contexts.first?.url { root?.importPairing(url) }
    }
    func sceneDidBecomeActive(_ scene: UIScene) {
        for error in NativeIdentity.retryPendingCleanup() + HostSettings.retryPendingCleanup() {
            NSLog("Deferred credential cleanup: %@",error)
        }
        root?.setSceneActive(true)
    }
    func sceneWillResignActive(_ scene: UIScene) { root?.setSceneActive(false) }
    func sceneDidEnterBackground(_ scene: UIScene) { root?.surface?.setActive(false) }
    func sceneDidDisconnect(_ scene: UIScene) { root?.surface?.close() }
}

@MainActor
final class RuneController: UIViewController, UIAdaptivePresentationControllerDelegate {
    private(set) var surface: RuneView?
    private let probe = QualificationProbe(arguments:ProcessInfo.processInfo.arguments)
    private var started = false
    private var pendingLink: String?
    private var qualificationTask: Task<Void,Never>?
    /// What the surface's edges show; UIKit continues it under the status
    /// bar and home indicator so the terminal is not framed by system chrome.
    private static let launchGround = UIColor(named: "LaunchGround") ?? .black
    private var ground = RuneController.launchGround
    private let empty = EmptyState()
    isolated deinit { qualificationTask?.cancel() }
    private var groundIsDark: Bool {
        var (red, green, blue): (CGFloat, CGFloat, CGFloat) = (0, 0, 0)
        ground.getRed(&red, green: &green, blue: &blue, alpha: nil)
        return 0.2126 * red + 0.7152 * green + 0.0722 * blue < 0.5
    }
    override var preferredStatusBarStyle: UIStatusBarStyle { groundIsDark ? .lightContent : .darkContent }
    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = ground
        empty.frame = view.bounds
        empty.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        empty.isHidden = true // Until launch knows there is no saved computer.
        view.addSubview(empty)
    }
    private func paint(_ ground: UIColor) {
        guard ground != self.ground else { return }
        self.ground = ground
        view.backgroundColor = ground
        setNeedsStatusBarAppearanceUpdate()
        surface?.editor.match(dark: groundIsDark)
    }
    private func showDisconnected() {
        empty.show(title: "No computer connected",
            detail: "Pair this phone with a computer running Phux to open its terminals here.",
            primary: "Connect a computer") { [weak self] in self?.showConnection() }
    }
    /// A surface that cannot open or draw is replaced, not left dead under
    /// an alert: retrying builds a new one from the saved computer.
    private func showFailure(_ message: String) {
        surface?.close(); surface?.removeFromSuperview(); surface = nil
        paint(Self.launchGround)
        empty.show(title: "Couldn't open the terminal", detail: message, primary: "Try Again",
            action: { [weak self] in self?.reopen() },
            secondary: "Change computer", other: { [weak self] in self?.showConnection() })
    }
    private func reopen() {
        do {
            if let configuration = try HostSettings.load() { open(configuration) }
            else { showConnection() }
        } catch { showFailure(error.localizedDescription) }
    }
    override func viewDidAppear(_ animated:Bool) {
        super.viewDidAppear(animated)
        guard !started else { return }
        started = true
        if let probe {
            qualify(probe)
            return
        }
        if let link = pendingLink {
            pendingLink = nil
            connection(link: link)
            return
        }
        reopen()
    }

    private func qualify(_ probe: QualificationProbe) {
        qualificationTask = Task { [weak self] in
            do {
                try await probe.testInputContracts()
                let configuration = try await probe.prepareConnection()
                guard !Task.isCancelled, let self else { return }
                self.open(configuration)
            } catch { probe.failure(error.localizedDescription); self?.show(error.localizedDescription) }
        }
    }
    func importPairing(_ url: URL) {
        guard url.scheme == "phux-rune" else { return }
        guard started else { pendingLink = url.absoluteString; return }
        connection(link: url.absoluteString)
    }

    private func showConnection() { connection(link: nil) }

    func setSceneActive(_ active: Bool) {
        // A native setup sheet must own input even after returning from a
        // camera permission prompt or Tailscale's approval browser. Without
        // one, a passing interruption (Control Center, a banner) keeps the
        // keyboard it would only bring straight back.
        let covered = presentedViewController is UINavigationController
        surface?.setActive(active && !covered, keepingKeyboard: !covered)
    }

    func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
        setupClosed()
    }

    /// Setup went away without pairing. Opened by a pairing link at launch,
    /// it came before the saved computer did: backing out returns to it.
    private func setupClosed() {
        if surface == nil, let saved = try? HostSettings.load() { open(saved) }
        setSceneActive(UIApplication.shared.applicationState == .active)
    }

    private func connection(link: String?) {
        if let nav = presentedViewController as? UINavigationController,
           let form = nav.viewControllers.first as? OnboardingController {
            guard link != nil else { return }
            nav.popToRootViewController(animated: false)
            deliver(link, to: form, navigation: nav)
            return
        }
        if surface == nil, empty.isHidden { showDisconnected() }
        // Unreadable saved settings must not block pairing again.
        let form = OnboardingController(values: try? HostSettings.load())
        form.closed = { [weak self] in self?.setupClosed() }
        form.connected = { [weak self] configuration in
            self?.dismiss(animated:true) { self?.open(configuration) }
        }
        let nav = UINavigationController(rootViewController: form)
        if let link { form.receive(link) }
        surface?.setActive(false)
        present(nav, animated:true)
        nav.presentationController?.delegate = self
    }

    private func deliver(_ link: String?, to form: OnboardingController, navigation: UINavigationController) {
        guard let link else { return }
        if navigation.presentedViewController != nil {
            navigation.dismiss(animated: false) { form.receive(link) }
        } else { form.receive(link) }
    }
    private func show(_ message:String) {
        let alert = UIAlertController.dark(title: "Phux Rune couldn't start", message: message)
        alert.addAction(UIAlertAction(title:"OK",style:.default))
        present(alert,animated:true)
    }
    private func open(_ saved: [String: Any]) {
        surface?.close(); surface?.removeFromSuperview()
        // Words only: what banners call the computer. A tailnet name reads
        // best as its first label; an address stays whole.
        var configuration = saved
        if let name = HostSettings.computerName {
            let label = name.split(separator: ".").first.map(String.init) ?? name
            let hostname = name.contains(where: \.isLetter) && !name.contains(" ")
            configuration["name"] = hostname ? label : name
        }
        let surface = RuneView(configuration:configuration)
        surface.qualification = probe
        surface.failed = { [weak self] message in
            self?.probe?.failure(message)
            self?.showFailure(message)
        }
        surface.editor.match(dark: groundIsDark)
        surface.connectionRequested = { [weak self] in self?.showConnection() }
        surface.groundChanged = { [weak self] ground in self?.paint(ground) }
        empty.isHidden = true
        surface.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(surface)
        NSLayoutConstraint.activate([
            surface.topAnchor.constraint(equalTo:view.safeAreaLayoutGuide.topAnchor),
            surface.leadingAnchor.constraint(equalTo:view.safeAreaLayoutGuide.leadingAnchor),
            surface.trailingAnchor.constraint(equalTo:view.safeAreaLayoutGuide.trailingAnchor),
            surface.bottomAnchor.constraint(equalTo:view.keyboardLayoutGuide.topAnchor)])
        self.surface = surface
    }
}

/// What the root shows without a surface, so dismissing setup or a failed
/// open never leaves a blank screen. Dark like the launch screen.
@MainActor
private final class EmptyState: UIView {
    private let body = UIContentUnavailableView(configuration: .empty())
    init() {
        super.init(frame: .zero)
        overrideUserInterfaceStyle = .dark
        body.frame = bounds
        body.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        addSubview(body)
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    func show(title: String, detail: String, primary: String, action: @escaping () -> Void,
              secondary: String? = nil, other: (() -> Void)? = nil) {
        var content = UIContentUnavailableConfiguration.empty()
        content.image = UIImage(systemName: "desktopcomputer")
        content.text = title
        content.secondaryText = detail
        content.button = .borderedProminent()
        content.button.title = primary
        content.buttonProperties.primaryAction = UIAction { _ in action() }
        if let secondary, let other {
            content.secondaryButton = .bordered()
            content.secondaryButton.title = secondary
            content.secondaryButtonProperties.primaryAction = UIAction { _ in other() }
        }
        body.configuration = content
        isHidden = false
    }
}
