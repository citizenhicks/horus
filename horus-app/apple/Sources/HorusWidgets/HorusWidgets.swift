import ActivityKit
import SwiftUI
import WidgetKit

@main
struct HorusWidgets: WidgetBundle {
    var body: some Widget {
        HorusChatsActivity()
    }
}

/// The palette, restated rather than shared.
///
/// The extension is its own process and its own binary: pulling `HorusStyle` in would drag
/// the app's glass controls and its whole glyph catalog across for four colours.
private enum ActivityPalette {
    static let accent = Color(red: 0.369, green: 0.506, blue: 0.675)
    static let signal = Color(red: 0.639, green: 0.745, blue: 0.549)
    static let warning = Color(red: 0.922, green: 0.796, blue: 0.545)
    static let muted = Color(red: 0.541, green: 0.588, blue: 0.671)
}

struct HorusChatsActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: HorusActivityAttributes.self) { context in
            LiveActivityView(state: context.state, isStale: context.isStale)
                .activityBackgroundTint(.black.opacity(0.55))
                .activitySystemActionForegroundColor(ActivityPalette.accent)
                .widgetURL(URL(string: "horus://chats"))
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    ActivityOrb(working: context.state.isWorking)
                        .frame(width: 38, height: 38)
                        .accessibilityHidden(true)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text(headline(context.state, isStale: context.isStale))
                        .font(.caption2)
                        .foregroundStyle(ActivityPalette.muted)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    ChatList(chats: context.state.chats)
                }
            } compactLeading: {
                ActivityOrb(working: context.state.isWorking)
                    .frame(width: 18, height: 18)
                    .accessibilityHidden(true)
            } compactTrailing: {
                CompactTally(state: context.state, isStale: context.isStale)
            } minimal: {
                ActivityOrb(working: context.state.isWorking)
                    .frame(width: 18, height: 18)
                    .accessibilityLabel(headline(context.state, isStale: context.isStale))
            }
            .widgetURL(URL(string: "horus://chats"))
            .keylineTint(ActivityPalette.accent)
        }
    }
}

/// The lock screen banner. Same content as the expanded island, with room to breathe.
private struct LiveActivityView: View {
    let state: HorusActivityAttributes.ContentState
    let isStale: Bool

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(spacing: 6) {
                ActivityOrb(working: state.isWorking)
                    .frame(width: 40, height: 40)
                    .accessibilityHidden(true)
                Spacer(minLength: 0)
            }
            VStack(alignment: .leading, spacing: 8) {
                Text(headline(state, isStale: isStale))
                    .font(.caption.weight(.medium))
                    .foregroundStyle(ActivityPalette.muted)
                ChatList(chats: state.chats)
            }
        }
        .padding(14)
    }
}

/// At most three chats — the cap is applied when the snapshot is built, not here.
private struct ChatList: View {
    let chats: [HorusChatSnapshot]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(chats) { chat in
                ChatRow(chat: chat)
            }
        }
    }
}

private struct ChatRow: View {
    let chat: HorusChatSnapshot

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(standingColor)
                .frame(width: 6, height: 6)
            VStack(alignment: .leading, spacing: 1) {
                Text(chat.title)
                    .font(.caption.weight(.medium))
                    .lineLimit(1)
                if !detail.isEmpty {
                    Text(detail)
                        .font(.caption2)
                        .foregroundStyle(ActivityPalette.muted)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 6)
            // The only value the activity can keep current on its own: everything else is
            // frozen at the last update the app managed to send.
            if let startedAt = chat.startedAt, chat.standing == .running {
                Text(startedAt, style: .timer)
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(ActivityPalette.muted)
                    .frame(maxWidth: 52, alignment: .trailing)
            }
        }
        .privacySensitive()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityDescription)
    }

    private var detail: String {
        let tokens = chat.tokens > 0 ? formattedTokens : ""
        return [chat.workspace, tokens].filter { !$0.isEmpty }.joined(separator: " · ")
    }

    private var formattedTokens: String {
        chat.tokens >= 1000
            ? "\(chat.tokens / 1000)k tokens"
            : "\(chat.tokens) tokens"
    }

    private var standingColor: Color {
        switch chat.standing {
        case .running: ActivityPalette.accent
        case .awaitingApproval: ActivityPalette.warning
        case .unread: ActivityPalette.signal
        }
    }

    private var accessibilityDescription: String {
        let status = switch chat.standing {
        case .running: "Running"
        case .awaitingApproval: "Needs approval"
        case .unread: "Unread result"
        }
        return [status, chat.title, detail].filter { !$0.isEmpty }.joined(separator: ", ")
    }
}

/// The compact island has room for a count and nothing else.
private struct CompactTally: View {
    let state: HorusActivityAttributes.ContentState
    let isStale: Bool

    var body: some View {
        Text("\(max(state.runningCount + state.attentionCount, 1))")
            .font(.caption2.weight(.semibold).monospacedDigit())
            .foregroundStyle(state.attentionCount > 0 ? ActivityPalette.warning : ActivityPalette.accent)
            .accessibilityLabel(headline(state, isStale: isStale))
    }
}

/// The composing orb, drawn once.
///
/// A Live Activity is rendered out of process from a snapshot, so the app's `TimelineView`
/// version cannot turn here — WidgetKit caps animation at a couple of seconds and disables
/// it outright on an always-on display. The orb is drawn at a fixed phase and says whether
/// work is happening through its tint; the elapsed timer is what actually moves.
private struct ActivityOrb: View {
    let working: Bool

    var body: some View {
        Canvas { context, size in
            let source = CGFloat(HorusComposingOrbRenderer.size)
            let scale = min(size.width, size.height) / source
            context.translateBy(
                x: (size.width - source * scale) / 2,
                y: (size.height - source * scale) / 2
            )
            context.scaleBy(x: scale, y: scale)
            for dot in HorusComposingOrbRenderer.dots(at: 0.6) {
                // A dot that lands under a pixel disappears; the island is small enough
                // that every orb here needs the floor.
                let radius = max(dot.radius, 0.5 / scale)
                let rect = CGRect(
                    x: dot.x - radius,
                    y: dot.y - radius,
                    width: radius * 2,
                    height: radius * 2
                )
                context.fill(Path(ellipseIn: rect), with: .color(tint.opacity(dot.opacity)))
            }
        }
    }

    private var tint: Color {
        working ? ActivityPalette.accent : ActivityPalette.muted
    }
}

private func headline(
    _ state: HorusActivityAttributes.ContentState,
    isStale: Bool
) -> String {
    isStale ? "Open Horus to refresh" : state.headline
}
