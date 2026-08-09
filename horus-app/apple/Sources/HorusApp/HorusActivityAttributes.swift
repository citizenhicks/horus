import Foundation
#if os(iOS)
import ActivityKit
#endif

/// One chat as the Dynamic Island and the Live Activity draw it.
///
/// Shared with the widget extension, so it carries no app types and no gateway types: the
/// extension is a separate process that never talks to a gateway, and everything it shows
/// has to survive a trip through `Codable`.
struct HorusChatSnapshot: Codable, Hashable, Identifiable, Sendable {
    enum Standing: String, Codable, Sendable {
        case running
        case awaitingApproval
        case unread
    }

    let id: String
    let title: String
    let workspace: String
    let tokens: Int
    /// When the current run began. The activity ticks the elapsed time from this itself,
    /// which is the one thing it can keep current while the app is suspended.
    let startedAt: Date?
    let standing: Standing
}

extension HorusChatSnapshot {
    static func bounded(_ raw: String, utf8Limit: Int) -> String {
        guard utf8Limit > 0 else { return "" }
        var result = ""
        var byteCount = 0
        for character in raw {
            let bytes = String(character).utf8.count
            guard byteCount + bytes <= utf8Limit else { break }
            result.append(character)
            byteCount += bytes
        }
        return result
    }

    /// A title long enough to recognise the chat by and short enough for the island.
    static func shortTitle(_ raw: String, limit: Int = 28) -> String {
        let trimmed = raw
            .replacingOccurrences(of: "\n", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count > limit else { return trimmed }
        // Cutting mid-word reads as a glitch; the last space before the limit does not.
        // A cut that already lands on a word boundary keeps that last word rather than
        // backing off over it.
        let clipped = trimmed.prefix(limit)
        let stem = trimmed.dropFirst(limit).first == " "
            ? clipped
            : clipped.lastIndex(of: " ").map { clipped[..<$0] } ?? clipped
        return stem.trimmingCharacters(in: .whitespaces) + "…"
    }

    static func shortWorkspace(_ raw: String?) -> String {
        guard let raw else { return "" }
        let component = raw.split { $0 == "/" || $0 == "\\" }.last.map(String.init) ?? raw
        return bounded(component, utf8Limit: 48)
    }
}

/// The static half of the activity. There is one activity for the whole app rather than one
/// per chat: the island shows a single item, and several chats commonly run at once.
#if os(iOS)
struct HorusActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        var chats: [HorusChatSnapshot]
        /// Totals count every chat, not just the ones that fit in `chats`.
        var runningCount: Int
        var attentionCount: Int

        var isWorking: Bool { runningCount > 0 }

        /// What the compact island says when there is no room for a list.
        var headline: String {
            if runningCount > 0, attentionCount > 0 {
                return "\(runningCount) running · \(attentionCount) need attention"
            }
            if runningCount > 0 {
                return runningCount == 1 ? "1 chat running" : "\(runningCount) chats running"
            }
            if attentionCount > 0 {
                return attentionCount == 1
                    ? "1 chat needs attention"
                    : "\(attentionCount) chats need attention"
            }
            return "Idle"
        }
    }

    /// Named so several gateways stay tellable apart in the activity.
    var gateway: String
}
#endif
