import ActivityKit
import UIKit
import WidgetKit

/// Keeps the Lock Screen card, the Dynamic Island and the widgets on what
/// Rune last said about its agents. Nothing updates them while the app is
/// suspended, so each says when it was taken and the card goes stale.
@MainActor
final class AgentCard {
    static let shared = AgentCard()
    /// A reconnect relists its agents one by one: show where it settles.
    private static let settle: TimeInterval = 1
    private nonisolated static let fresh: TimeInterval = 15 * 60
    private var latest = AgentPresenceStore.read()
    private var shown: AgentPresence?
    private var work: DispatchWorkItem?
    /// ActivityKit's calls are async; one at a time keeps them in order.
    private var queue: Task<Void, Never>?

    /// A frame's `presence`, when it moved.
    func update(_ value: Any?) {
        guard var fields = value as? [String: Any] else { return }
        fields["at"] = Date.now.timeIntervalSince1970
        guard let data = try? JSONSerialization.data(withJSONObject: fields),
              let presence = try? JSONDecoder().decode(AgentPresence.self, from: data) else {
            NSLog("Rune's agent presence is not one this app reads.")
            return
        }
        latest = presence
        work?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.show() }
        self.work = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.settle, execute: work)
    }

    /// Leaving the foreground: the last moment an activity may start, and
    /// what it shows is true as of now when `connected`.
    func leave(connected: Bool) {
        work?.cancel(); work = nil
        if connected { latest?.at = Date.now.timeIntervalSince1970 }
        let background = UIApplication.shared.beginBackgroundTask()
        show()
        let shown = queue
        Task {
            await shown?.value
            UIApplication.shared.endBackgroundTask(background)
        }
    }

    /// Another computer: nothing shown is about it.
    func forget() {
        work?.cancel(); work = nil
        latest = nil
        show()
    }

    private func show() {
        guard latest != shown else { return }
        shown = latest
        AgentPresenceStore.write(latest)
        WidgetCenter.shared.reloadTimelines(ofKind: AgentPresenceStore.widgetKind)
        let next = latest
        let previous = queue
        queue = Task {
            await previous?.value
            await Self.present(next)
        }
    }

    private nonisolated static func present(_ presence: AgentPresence?) async {
        var running = Activity<AgentActivity>.activities.filter {
            $0.activityState == .active || $0.activityState == .stale
        }
        guard let presence, !presence.quiet else {
            for activity in running { await activity.end(nil, dismissalPolicy: .immediate) }
            return
        }
        let content = ActivityContent(state: presence, staleDate: presence.date.addingTimeInterval(fresh),
                                      relevanceScore: Double(presence.waiting))
        if let activity = running.popLast() {
            await activity.update(content)
            for extra in running { await extra.end(nil, dismissalPolicy: .immediate) }
            return
        }
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        do {
            let activity = try Activity.request(attributes: AgentActivity(), content: content,
                                                pushType: LivePush.gateway == nil ? nil : .token)
            await LivePush.follow(activity)
        }
        catch { NSLog("The Lock Screen card did not start: %@", error.localizedDescription) }
    }
}
