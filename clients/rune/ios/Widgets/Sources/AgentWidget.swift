import SwiftUI
import WidgetKit

@main
struct PhuxWidgets: WidgetBundle {
    var body: some Widget {
        AgentLiveActivity()
        AgentWidget()
    }
}

struct AgentEntry: TimelineEntry {
    let date: Date
    /// Nil until the app has connected once on this phone.
    let presence: AgentPresence?
    var relevance: TimelineEntryRelevance? {
        presence.map { TimelineEntryRelevance(score: Float($0.waiting)) }
    }
}

struct AgentProvider: TimelineProvider {
    private static let sample = AgentPresence(
        waiting: 2, working: 3, headline: "2 need you", host: "studio",
        rows: [.init(title: "Choose the transport", detail: "claude · phux", state: "needs_you", link: ""),
               .init(title: "Write the migration", detail: "codex · site", state: "done", link: ""),
               .init(title: "Fix the flaky test", detail: "claude · phux", state: "working", link: "")],
        at: Date.now.timeIntervalSince1970)

    func placeholder(in _: Context) -> AgentEntry { AgentEntry(date: .now, presence: Self.sample) }
    func getSnapshot(in context: Context, completion: @escaping (AgentEntry) -> Void) {
        completion(AgentEntry(date: .now, presence: AgentPresenceStore.read() ?? (context.isPreview ? Self.sample : nil)))
    }
    /// The app reloads this kind when its agents move; between, nothing does.
    func getTimeline(in _: Context, completion: @escaping (Timeline<AgentEntry>) -> Void) {
        completion(Timeline(entries: [AgentEntry(date: .now, presence: AgentPresenceStore.read())], policy: .never))
    }
}

struct AgentWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: AgentPresenceStore.widgetKind, provider: AgentProvider()) { entry in
            AgentWidgetView(presence: entry.presence)
                .containerBackground(.background, for: .widget)
                .widgetURL(entry.presence?.link ?? AgentPresence.inbox)
        }
        .configurationDisplayName("Agents")
        .description("Who is waiting on you and how many agents are working.")
        .supportedFamilies([.accessoryInline, .accessoryCircular, .accessoryRectangular,
                            .systemSmall, .systemMedium])
    }
}

struct AgentWidgetView: View {
    @Environment(\.widgetFamily) private var family
    let presence: AgentPresence?

    var body: some View {
        switch family {
        case .accessoryInline:
            Text(verbatim: words)
        case .accessoryCircular:
            ZStack {
                AccessoryWidgetBackground()
                VStack(spacing: 2) {
                    eyes.frame(height: 13)
                    if let presence, !presence.quiet { Count(presence: presence, stale: false) }
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(verbatim: words))
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) { eyes.frame(height: 12); Text(verbatim: words).font(.headline) }
                ForEach((presence?.rows ?? []).prefix(2), id: \.self) { row in
                    Text(verbatim: row.title).font(.caption).lineLimit(1).privacySensitive()
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        case .systemSmall:
            VStack(alignment: .leading, spacing: 8) {
                eyes.frame(height: 24)
                Spacer(minLength: 0)
                headline
                asOf
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        default:
            VStack(alignment: .leading, spacing: 7) {
                HStack(spacing: 10) { eyes.frame(height: 22); headline; Spacer(minLength: 4); asOf }
                if let presence { AgentRows(presence: presence, limit: 3) }
                Spacer(minLength: 0)
            }
        }
    }

    private var eyes: some View {
        presence.map { FoxEyes($0) } ?? FoxEyes(.asleep)
    }

    @ViewBuilder private var asOf: some View {
        if let presence { AsOf(presence: presence) }
    }

    @ViewBuilder private var headline: some View {
        if let presence, !presence.quiet {
            Headline(presence: presence)
        } else {
            Text(verbatim: words).font(.headline)
        }
    }

    private var words: String {
        guard let presence else { return "Open phux" }
        return presence.quiet ? "All quiet" : presence.headline
    }
}
