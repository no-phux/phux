import ActivityKit
import Foundation

/// What the phone shows while the app is closed, as of the app's last look:
/// Rune's `Presence` (clients/rune/src/desk/presence.rs) and when it was
/// taken. Compiled into the app, which writes it, and the widget extension.
struct AgentPresence: Codable, Hashable, Sendable {
    struct Row: Codable, Hashable, Sendable {
        var title: String
        var detail: String
        var state: String
        var link: String
    }
    var waiting: Int
    var working: Int
    var headline: String
    var host: String
    var rows: [Row]
    /// Seconds since 1970.
    var at: Double

    var quiet: Bool { waiting == 0 && working == 0 }
    var date: Date { Date(timeIntervalSince1970: at) }
    /// What a tap off any row opens: the one pane that waits, else the
    /// inbox. A pushed update carries no rows, so it opens the inbox.
    var link: URL? {
        (waiting == 1 ? rows.first.flatMap { URL(string: $0.link) } : nil) ?? Self.inbox
    }
    static let inbox = URL(string: "phux://inbox")
}

struct AgentActivity: ActivityAttributes {
    typealias ContentState = AgentPresence
}

/// The presence in the App Group the app and its widgets share. Both bundles
/// name the group (build.py `--app-group`); one without a group has no
/// widgets, so there is nothing to keep.
enum AgentPresenceStore {
    /// The kind the app has always shipped: a widget a person already placed
    /// stays in place.
    static let widgetKind = "dev.phux.mobile.agentstatus"

    private static var file: URL? {
        guard let group = Bundle.main.object(forInfoDictionaryKey: "PhuxAppGroup") as? String else { return nil }
        return FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)?
            .appending(path: "agent-presence.json")
    }

    static func read() -> AgentPresence? {
        guard let file, let data = try? Data(contentsOf: file) else { return nil }
        return try? JSONDecoder().decode(AgentPresence.self, from: data)
    }

    /// Readable by a widget on a locked phone, once unlocked since it started.
    static func write(_ presence: AgentPresence?) {
        guard let file else { return }
        do {
            guard let presence else {
                if FileManager.default.fileExists(atPath: file.path) { try FileManager.default.removeItem(at: file) }
                return
            }
            try JSONEncoder().encode(presence).write(
                to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch { NSLog("Agent presence was not saved: %@", error.localizedDescription) }
    }
}
