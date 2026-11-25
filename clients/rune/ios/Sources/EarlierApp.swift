import UIKit
import UserNotifications

/// What an earlier app left on the phone under this bundle identifier: an
/// update replaces the code, not the system's record of it.
@MainActor
enum EarlierApp {
    /// UIKit opens a restored scene with the delegate class saved with its
    /// session and never asks `configurationForConnecting`. An earlier app's
    /// class then owns the window: a black screen, and a crash on leaving.
    /// Forget those sessions before UIKit reads them. Nothing here restores
    /// from a session, so forgetting one loses nothing.
    static func forgetScenes(library: URL = .libraryDirectory,
                             bundle: String = Bundle.main.bundleIdentifier ?? "") {
        let known = library.appending(path: "Saved Application State/\(bundle).savedState/KnownSceneSessions")
        guard let saved = try? Data(contentsOf: known.appending(path: "data.data")),
              saved.range(of: Data(NSStringFromClass(SceneDelegate.self).utf8)) == nil else { return }
        do { try FileManager.default.removeItem(at: known) }
        catch { NSLog("Earlier scene sessions were not removed: %@", error.localizedDescription) }
    }

    /// Where those sessions live is UIKit's own business. Should a scene still
    /// connect with another class, give it ours: the window must open.
    static func claim(_ scene: UIScene) {
        guard let scene = scene as? UIWindowScene, scene.session.role == .windowApplication,
              !(scene.delegate is SceneDelegate) else { return }
        NSLog("Replacing an earlier app's scene delegate.")
        let delegate = SceneDelegate()
        scene.delegate = delegate
        delegate.open(scene, url: nil)
    }

    /// This app sets no badge: one on its icon is the earlier app's, and
    /// nothing else would ever clear it.
    static func clearBadge() { UNUserNotificationCenter.current().setBadgeCount(0) }
}
