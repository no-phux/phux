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
        let window = UIWindow(windowScene:scene)
        window.rootViewController = UINavigationController(rootViewController:controller)
        self.window = window
        window.makeKeyAndVisible()
    }
    func sceneDidBecomeActive(_ scene: UIScene) {
        for error in NativeIdentity.retryPendingCleanup() + HostSettings.retryPendingCleanup() {
            NSLog("Deferred credential cleanup: %@",error)
        }
        root?.surface?.setActive(true)
    }
    func sceneWillResignActive(_ scene: UIScene) { root?.surface?.setActive(false) }
    func sceneDidDisconnect(_ scene: UIScene) { root?.surface?.close() }
}

@MainActor
final class RuneController: UIViewController {
    private(set) var surface: RuneView?
    private let probe = QualificationProbe(arguments:ProcessInfo.processInfo.arguments)
    private var started = false
    private var qualificationTask: Task<Void,Never>?
    isolated deinit { qualificationTask?.cancel() }
    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Phux Rune"
        view.backgroundColor = .systemBackground
        navigationItem.rightBarButtonItem = UIBarButtonItem(title:"Connection", style:.plain,
            target:self, action:#selector(connection))
    }
    override func viewDidAppear(_ animated:Bool) {
        super.viewDidAppear(animated)
        guard !started else { return }
        started = true
        if let probe {
            qualificationTask = Task { [weak self] in
                do {
                    try probe.testInputContracts()
                    var configuration = try await NativeIdentity.enroll(configuration:probe.configuration,
                        ticket:probe.enrollmentTicket)
                    if let endpoint = probe.connectionEndpoint { configuration["endpoint"] = endpoint }
                    guard !Task.isCancelled, let self else {
                        try NativeIdentity.discard(configuration:configuration); return
                    }
                    try HostSettings.save(configuration)
                    self.open(configuration)
                } catch { probe.failure(error.localizedDescription); self?.show(error.localizedDescription) }
            }
            return
        }
        do {
            if let configuration = try HostSettings.load() { open(configuration) }
            else { connection() }
        } catch { show(error.localizedDescription) }
    }
    @objc private func connection() {
        let values: [String:Any]?
        do { values = try HostSettings.load() } catch { show(error.localizedDescription); return }
        let form = ConnectionForm(values:values)
        form.connected = { [weak self] configuration in
            self?.dismiss(animated:true) { self?.open(configuration) }
        }
        present(UINavigationController(rootViewController:form),animated:true)
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
            alert.addAction(UIAlertAction(title:"Connection",style:.default) { _ in self?.connection() })
            self?.present(alert,animated:true)
        }
        surface.connectionRequested = { [weak self] in self?.connection() }
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
