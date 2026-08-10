#if os(iOS)
@preconcurrency import ActivityKit
import Foundation

/// Mirrors the chat list into one Live Activity.
///
/// One activity for the whole app rather than one per chat: the Dynamic Island only ever
/// shows a single item, and several chats commonly run at once — a per-chat activity would
/// make them fight over it.
///
/// The activity only stays current while the app is awake. ActivityKit can be driven from a
/// push, but nothing here holds a push token: once iOS suspends the app the socket stops
/// delivering, and the activity keeps showing the last state it was handed. Its elapsed
/// timers keep counting because the system owns them.
@MainActor
final class LiveActivityController {
    private static let freshness: TimeInterval = 15 * 60

    private var activity: Activity<HorusActivityAttributes>?
    private var operationTask: Task<Void, Never>?

    func update(
        sessions: [SessionRecord],
        unread: Set<String>,
        gateway: String,
        titleOverrides: [String: String] = [:]
    ) {
        let chats = HorusChatSnapshot.live(
            sessions: sessions,
            unread: unread,
            titleOverrides: titleOverrides
        )
        let tallies = HorusChatSnapshot.tallies(sessions: sessions, unread: unread)
        let state = HorusActivityAttributes.ContentState(
            chats: chats,
            runningCount: tallies.running,
            attentionCount: tallies.attention
        )
        let gateway = HorusChatSnapshot.bounded(gateway, utf8Limit: 64)
        let previous = operationTask
        operationTask = Task { [weak self] in
            await previous?.value
            guard !Task.isCancelled, let self else { return }
            await self.apply(state: state, gateway: gateway)
        }
    }

    func end() {
        let previous = operationTask
        operationTask = Task { [weak self] in
            await previous?.value
            guard !Task.isCancelled, let self else { return }
            await self.endAll()
        }
    }

    private func apply(
        state: HorusActivityAttributes.ContentState,
        gateway: String
    ) async {
        guard ActivityAuthorizationInfo().areActivitiesEnabled,
              !state.chats.isEmpty
        else {
            await endAll()
            return
        }

        let activities = Activity<HorusActivityAttributes>.activities
        let current = activities.first { $0.attributes.gateway == gateway }
        for candidate in activities where candidate.id != current?.id {
            await candidate.end(nil, dismissalPolicy: .immediate)
        }
        if let current {
            activity = current
            await current.update(content(for: state))
            return
        }

        do {
            activity = try Activity.request(
                attributes: HorusActivityAttributes(gateway: gateway),
                content: content(for: state)
            )
        } catch {
            // Requesting can fail for reasons the app cannot fix — the user disabled Live
            // Activities for Horus, or too many are already on screen. Neither is worth a
            // toast in the middle of a turn.
            activity = nil
        }
    }

    private func endAll() async {
        var ended: Set<String> = []
        for candidate in Activity<HorusActivityAttributes>.activities {
            ended.insert(candidate.id)
            await candidate.end(nil, dismissalPolicy: .immediate)
        }
        if let activity, ended.insert(activity.id).inserted {
            await activity.end(nil, dismissalPolicy: .immediate)
        }
        activity = nil
    }

    private func content(
        for state: HorusActivityAttributes.ContentState
    ) -> ActivityContent<HorusActivityAttributes.ContentState> {
        ActivityContent(
            state: state,
            staleDate: Date().addingTimeInterval(Self.freshness)
        )
    }
}
#endif
