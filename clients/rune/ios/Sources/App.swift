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
    private var ground = UIColor(named: "LaunchGround") ?? .black
    private lazy var empty = EmptyState { [weak self] in self?.showConnection() }
    isolated deinit { qualificationTask?.cancel() }
    override var preferredStatusBarStyle: UIStatusBarStyle {
        var (red, green, blue): (CGFloat, CGFloat, CGFloat) = (0, 0, 0)
        ground.getRed(&red, green: &green, blue: &blue, alpha: nil)
        return 0.2126 * red + 0.7152 * green + 0.0722 * blue < 0.5 ? .lightContent : .darkContent
    }
    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = ground
        empty.frame = view.bounds
        empty.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(empty)
    }
    private func paint(_ ground: UIColor) {
        guard ground != self.ground else { return }
        self.ground = ground
        view.backgroundColor = ground
        setNeedsStatusBarAppearanceUpdate()
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
        do {
            if let configuration = try HostSettings.load() { open(configuration) }
            else { showConnection() }
        } catch { show(error.localizedDescription) }
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
        // camera permission prompt or Tailscale's approval browser.
        surface?.setActive(active && !(presentedViewController is UINavigationController))
    }

    func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
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
        let values: [String:Any]?
        do { values = try HostSettings.load() } catch { show(error.localizedDescription); return }
        let form = OnboardingController(values:values)
        form.closed = { [weak self] in
            self?.setSceneActive(UIApplication.shared.applicationState == .active)
        }
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
        let alert = UIAlertController(title:"Connection",message:message,preferredStyle:.alert)
        alert.addAction(UIAlertAction(title:"OK",style:.default))
        present(alert,animated:true)
    }
    private func open(_ configuration: [String: Any]) {
        surface?.close(); surface?.removeFromSuperview()
        let surface = RuneView(configuration:configuration)
        surface.qualification = probe
        surface.failed = { [weak self] message in
            self?.probe?.failure(message)
            let alert = UIAlertController(title:"Could not open Rune",message:message,preferredStyle:.alert)
            alert.addAction(UIAlertAction(title:"Connection",style:.default) { _ in self?.showConnection() })
            self?.present(alert,animated:true)
        }
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

/// Shown only while no computer is connected, so dismissing first-run setup
/// never leaves a blank screen. Dark like the launch screen and the terminal.
@MainActor
private final class EmptyState: UIView {
    init(connect: @escaping () -> Void) {
        super.init(frame: .zero)
        overrideUserInterfaceStyle = .dark
        var content = UIContentUnavailableConfiguration.empty()
        content.image = UIImage(systemName: "desktopcomputer")
        content.text = "No computer connected"
        content.secondaryText = "Pair this phone with a computer running Phux to open its terminals here."
        content.button = .borderedProminent()
        content.button.title = "Connect a computer"
        content.buttonProperties.primaryAction = UIAction { _ in connect() }
        let body = UIContentUnavailableView(configuration: content)
        body.frame = bounds
        body.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        addSubview(body)
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
}
