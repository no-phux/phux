import ActivityKit
import SwiftUI
import WidgetKit

/// The Lock Screen card and the Dynamic Island.
struct AgentLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AgentActivity.self) { context in
            AgentLockScreenCard(presence: context.state, stale: context.isStale)
                .padding(14)
                .widgetURL(context.state.link)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    FoxEyes(context.state, stale: context.isStale)
                        .frame(height: 22)
                        .padding(.leading, 6)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    AsOf(presence: context.state).padding(.trailing, 6)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 6) {
                        Headline(presence: context.state)
                        AgentRows(presence: context.state, limit: 2)
                    }
                    .padding(.horizontal, 6)
                }
            } compactLeading: {
                FoxEyes(context.state, stale: context.isStale).frame(height: 14).padding(.leading, 2)
            } compactTrailing: {
                Count(presence: context.state, stale: context.isStale)
            } minimal: {
                FoxEyes(context.state, stale: context.isStale).padding(1)
            }
            .widgetURL(context.state.link)
            .keylineTint(context.state.waiting > 0 ? Color("StatusNeedsYou") : nil)
        }
    }
}

struct AgentLockScreenCard: View {
    let presence: AgentPresence
    let stale: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                FoxEyes(presence, stale: stale).frame(height: 20)
                Headline(presence: presence)
                Spacer(minLength: 4)
                AsOf(presence: presence)
            }
            AgentRows(presence: presence, limit: 3)
        }
    }
}

/// "2 need you", and beside it what else is going on and where.
struct Headline: View {
    let presence: AgentPresence

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(verbatim: presence.headline).font(.headline)
            if !aside.isEmpty {
                Text(verbatim: aside).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var aside: String {
        let working = presence.waiting > 0 && presence.working > 0 ? "\(presence.working) working" : ""
        return [working, presence.host].filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

/// When the app last looked. Nothing here moves until it looks again.
struct AsOf: View {
    let presence: AgentPresence

    var body: some View {
        Text("as of \(presence.date, style: .time)")
            .font(.caption2.monospacedDigit())
            .foregroundStyle(.secondary)
            .lineLimit(1)
    }
}

struct Count: View {
    let presence: AgentPresence
    let stale: Bool

    var body: some View {
        Text(presence.waiting > 0 ? presence.waiting : presence.working, format: .number)
            .font(.body.weight(.semibold).monospacedDigit())
            .contentTransition(.numericText())
            .foregroundStyle(stale ? Color.secondary : Color(presence.waiting > 0 ? "StatusNeedsYou" : "StatusWorking"))
            .accessibilityLabel(Text(verbatim: presence.headline))
    }
}

/// Who waits, then who works, each opening its pane. Names are the person's
/// own: a locked phone that hides previews hides these.
struct AgentRows: View {
    let presence: AgentPresence
    let limit: Int

    var body: some View {
        ForEach(presence.rows.prefix(limit), id: \.self) { row in
            if let url = URL(string: row.link) {
                Link(destination: url) { AgentRow(row: row) }
            } else { AgentRow(row: row) }
        }
    }
}

struct AgentRow: View {
    let row: AgentPresence.Row

    var body: some View {
        HStack(spacing: 7) {
            Circle().fill(ink).frame(width: 7, height: 7).widgetAccentable()
            Text(verbatim: row.title).font(.subheadline).lineLimit(1).layoutPriority(1)
            Spacer(minLength: 4)
            Text(verbatim: row.detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
        }
        .privacySensitive()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(verbatim: "\(row.title), \(word), \(row.detail)"))
    }

    private var ink: Color {
        switch row.state {
        case "working": Color("StatusWorking")
        case "failed": Color("StatusFailed")
        case "done": Color("StatusDone")
        default: Color("StatusNeedsYou")
        }
    }

    private var word: String { row.state.replacingOccurrences(of: "_", with: " ") }
}
