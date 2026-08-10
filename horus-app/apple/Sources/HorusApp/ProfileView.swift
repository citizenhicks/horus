import Foundation
import StoreKit
import SwiftUI

struct ProfileView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let usage = model.profile?.dailyUsage ?? []
        let providerLabels = model.providerStatuses.reduce(into: [String: String]()) {
            $0[$1.provider] = $1.label
        }
        PageScaffold(
            title: "Settings",
            detail: "",
            headerAccessory: SettingsInformationButton.init
        ) {
            Section("Usage") {
                CloudAllowanceStatus()
                ProfileUsageSection(days: usage)
                DisclosureGroup("Usage history") {
                    ProfileUsageHistory(days: usage, providerLabels: providerLabels)
                }
                if let stats = model.profile?.runStats {
                    DisclosureGroup("Run activity") {
                        ProfileRunStatsSection(stats: stats)
                        ProfileRecentRuns(groups: model.profile?.recentRunGroups ?? [])
                    }
                }
            }
            Section("Account") {
                CloudAccountSettings()
            }
            Section("Data & Privacy") {
                DataPrivacySettings()
            }
            Section("Appearance") {
                AppearanceSettings()
            }
            Section("Security") {
                AppLockSettings()
            }
        }
        .task(id: model.connectionState.isReady) { model.refreshProfile() }
    }
}

private struct SettingsInformationButton: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var showsInformation = false

    var body: some View {
        Button {
            showsInformation = true
        } label: {
            HorusIcon(.info, foreground: palette.muted)
        }
        .buttonStyle(HorusIconButtonStyle())
        .accessibilityLabel("About Horus")
        .accessibilityHint("Shows version, legal, and support information")
        .help("About Horus")
        .popover(
            isPresented: $showsInformation,
            attachmentAnchor: .rect(.bounds),
            arrowEdge: .top
        ) {
            VStack(alignment: .leading, spacing: 0) {
                Text(versionDescription)
                    .font(HorusStyle.controlFont)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.bottom, 16)
                Divider()
                SettingsInformationRow(
                    title: "Acceptable Use Policy",
                    glyph: .shieldCheck,
                    action: { showPlaceholder("Acceptable Use Policy") }
                )
                SettingsInformationRow(
                    title: "Terms of Service",
                    glyph: .doc,
                    action: { showPlaceholder("Terms of Service") }
                )
                SettingsInformationRow(
                    title: "Privacy Policy",
                    glyph: .shield02,
                    action: { showPlaceholder("Privacy Policy") }
                )
                SettingsInformationRow(
                    title: "Licenses",
                    glyph: .fileText,
                    action: { showPlaceholder("Licenses") }
                )
                Divider()
                SettingsInformationRow(
                    title: "Help & Support",
                    glyph: .question,
                    action: { showPlaceholder("Help & Support") }
                )
            }
            .padding(18)
            .frame(width: 320, alignment: .leading)
            .presentationCompactAdaptation(.popover)
        }
    }

    private var versionDescription: String {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")
            as? String ?? "—"
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "—"
        return "Horus v\(version) (\(build))"
    }

    private func showPlaceholder(_ title: String) {
        showsInformation = false
        model.showToast("\(title) will be available before the cloud release.")
    }
}

private struct SettingsInformationRow: View {
    @Environment(\.horusPalette) private var palette
    let title: String
    let glyph: HorusGlyph
    let action: @MainActor () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                HorusIcon(glyph, size: 18, foreground: palette.muted)
                Text(title)
                Spacer(minLength: 12)
                HorusIcon(.arrowUpRight01, size: 14, foreground: palette.muted)
            }
            .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize)
            .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
    }
}

private struct CloudAllowanceStatus: View {
    @Environment(\.horusPalette) private var palette

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text("Included Luna usage")
                    .font(HorusStyle.controlFont)
                Spacer(minLength: 8)
                HorusBadge(text: "Cloud required", glyph: .globe02)
            }
            Text("Cloud subscribers will see their remaining allowance and reset date here.")
                .font(.footnote)
                .foregroundStyle(palette.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }
}

private struct CloudAccountSettings: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var isRestoringPurchases = false
    @State private var showsSubscriptionManagement = false

    var body: some View {
        HStack(spacing: 12) {
            HorusIcon(.userFocus, size: 20, foreground: palette.muted)
            Text("Horus Cloud")
                .font(HorusStyle.controlFont)
            Spacer(minLength: 8)
            HorusBadge(text: "Not signed in", glyph: .xCircle)
        }
        .accessibilityElement(children: .combine)

        HorusCloudOfferButton()

        Button(
            isRestoringPurchases ? "Restoring Purchases" : "Restore Purchases",
            glyph: .arrowClockwise,
            action: restorePurchases
        )
        .disabled(isRestoringPurchases)

        Button("Manage Subscription", glyph: .sealCheck) {
            showsSubscriptionManagement = true
        }
        .accessibilityHint("Opens App Store subscription management, where you can unsubscribe")
        .manageSubscriptionsSheet(isPresented: $showsSubscriptionManagement)

        Button("Delete Account", glyph: .trash, role: .destructive) {}
            .disabled(true)

        Text("Account deletion and logout become available after a Horus Cloud account is connected.")
            .font(.footnote)
            .foregroundStyle(palette.muted)
    }

    private func restorePurchases() {
        guard !isRestoringPurchases else { return }
        isRestoringPurchases = true
        Task {
            defer { isRestoringPurchases = false }
            do {
                try await AppStore.sync()
                model.showToast("Purchase history refreshed.", tone: .success)
            } catch {
                model.showToast("Couldn’t restore purchases: \(error.localizedDescription)", tone: .error)
            }
        }
    }
}

private struct DataPrivacySettings: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        Button("View Horus Cloud Data", glyph: .hardDrives) {}
            .disabled(true)
        Button("Delete Horus Cloud Data", glyph: .trash, role: .destructive) {}
            .disabled(true)
        Text("Connect a cloud account to review, export, or permanently delete its stored data.")
            .font(.footnote)
            .foregroundStyle(palette.muted)

        Toggle("Help improve Horus", isOn: Binding(
            get: { model.sharesHorusDiagnostics },
            set: { model.setSharesHorusDiagnostics($0) }
        ))
        .toggleStyle(.switch)

        Text("Off by default. This preference is stored on this device; no diagnostics are sent until the cloud diagnostics pipeline is connected.")
            .font(.footnote)
            .foregroundStyle(palette.muted)
            .fixedSize(horizontal: false, vertical: true)
    }
}

private let profileUsageWeekCount = 25

private struct ProfileUsageSection: View {
    @Environment(\.horusPalette) private var palette
    let days: [DailyUsage]

    var body: some View {
        let total = days.reduce(into: TokenUsage()) { result, day in
            result.inputTokens += day.usage.inputTokens
            result.cachedInputTokens += day.usage.cachedInputTokens
            result.outputTokens += day.usage.outputTokens
            result.totalTokens += day.usage.totalTokens
        }
        VStack(alignment: .leading, spacing: 16) {
            Text("Recorded totals")
                .font(HorusStyle.controlFont)
                .foregroundStyle(palette.muted)
            // Four fixed columns: an adaptive grid drops to three and orphans the last metric.
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 8), count: 4), spacing: 16) {
                UsageMetric(label: "TOKENS", value: compact(total.totalTokens))
                UsageMetric(label: "INPUT", value: compact(total.inputTokens))
                UsageMetric(label: "OUTPUT", value: compact(total.outputTokens))
                UsageMetric(label: "CACHE", value: cacheHit(total))
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ProfileUsageHistory: View {
    @Environment(\.horusPalette) private var palette
    let days: [DailyUsage]
    let providerLabels: [String: String]

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .firstTextBaseline) {
                Text("Usage by provider")
                    .font(HorusStyle.controlFont)
                Spacer()
                Text("Last \(profileUsageWeekCount) weeks")
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
            }
            ProviderUsageChart(
                usage: days,
                providerLabels: providerLabels,
                weekCount: profileUsageWeekCount
            )
            Text("\(profileUsageWeekCount)-week activity")
                .font(HorusStyle.controlFont)
                .foregroundStyle(palette.muted)
            UsageHeatmap(days: days)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct UsageMetric: View {
    @Environment(\.horusPalette) private var palette
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(label)
                .font(HorusStyle.metadataFont.weight(.bold))
                .tracking(1)
                .foregroundStyle(palette.muted)
            Text(value)
                .font(.headline)
                .monospacedDigit()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ProfileRunStatsSection: View {
    let stats: RunStats

    var body: some View {
        VStack(spacing: 16) {
            HStack(spacing: 8) {
                UsageMetric(label: "RUNS", value: compact(stats.runCount))
                UsageMetric(label: "FAILED", value: compact(stats.failedRunCount))
                UsageMetric(label: "ABORTED", value: compact(stats.abortedRunCount))
                UsageMetric(label: "ELAPSED", value: formatMilliseconds(stats.elapsedMs))
            }
            HStack(spacing: 8) {
                UsageMetric(label: "MODEL CALLS", value: compact(stats.modelCalls))
                UsageMetric(label: "TOOL CALLS", value: compact(stats.toolCalls))
                UsageMetric(label: "TOOL ERRORS", value: compact(stats.failedToolCalls))
                UsageMetric(label: "RUN TOKENS", value: compact(stats.usage.totalTokens))
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ProfileRecentRuns: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var collapsedGroupIDs: Set<String> = []
    let groups: [SessionRunGroup]

    var body: some View {
        if groups.isEmpty {
            Text("No completed runs yet.")
                .font(HorusStyle.bodyFont)
                .foregroundStyle(palette.muted)
        } else {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(groups) { group in
                        DisclosureGroup(isExpanded: expansion(for: group.id)) {
                            ForEach(group.runs) { run in
                                Button {
                                    model.openSession(group.sessionId)
                                    model.destination = .chat
                                } label: {
                                    HStack(spacing: 10) {
                                        HorusIcon(runGlyph(run), foreground: runColor(run))
                                        VStack(alignment: .leading, spacing: 2) {
                                            HStack(spacing: 6) {
                                                Text(run.sessionId == group.sessionId ? "Run" : "Sub-run")
                                                    .font(HorusStyle.metadataFont.weight(.semibold))
                                                Text(
                                                    runDate(run),
                                                    format: .dateTime.month(.abbreviated).day().hour().minute()
                                                )
                                                .font(HorusStyle.metadataFont)
                                                .foregroundStyle(palette.muted)
                                            }
                                            Text(runDetail(run))
                                                .font(HorusStyle.metadataFont)
                                                .foregroundStyle(palette.muted)
                                                .lineLimit(1)
                                        }
                                        Spacer(minLength: 4)
                                        HorusIcon(.caretRight, size: 12, foreground: palette.muted)
                                    }
                                    .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize)
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.horusPlain)
                                .accessibilityLabel("\(runOutcome(run)), \(group.title)")
                                .accessibilityValue(runDetail(run))
                                .accessibilityHint("Opens the chat for this run")
                            }
                        } label: {
                            HStack(spacing: 6) {
                                Text(group.title)
                                    .font(HorusStyle.controlFont)
                                    .lineLimit(1)
                                Text(group.runs.count, format: .number)
                                    .font(HorusStyle.metadataFont)
                                    .foregroundStyle(palette.muted)
                            }
                            .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize)
                        }
                        .tint(palette.accent)
                    }
                }
            }
            .frame(height: CGFloat(min(visibleRowCount, 20)) * HorusStyle.iconButtonSize)
            .scrollBounceBehavior(.basedOnSize)
        }
    }

    private var visibleRowCount: Int {
        groups.reduce(0) { count, group in
            count + 1 + (collapsedGroupIDs.contains(group.id) ? 0 : group.runs.count)
        }
    }

    private func expansion(for groupID: String) -> Binding<Bool> {
        Binding(
            get: { !collapsedGroupIDs.contains(groupID) },
            set: { expanded in
                if expanded { collapsedGroupIDs.remove(groupID) }
                else { collapsedGroupIDs.insert(groupID) }
            }
        )
    }

    private func runDetail(_ run: RunSummary) -> String {
        "\(formatMilliseconds(run.elapsedMs)) · \(run.modelCalls) model · \(run.toolCalls) tools · \(compact(run.usage.totalTokens)) tokens"
    }

    private func runDate(_ run: RunSummary) -> Date {
        Date(timeIntervalSince1970: TimeInterval(run.startedAtMs) / 1_000)
    }

    private func runOutcome(_ run: RunSummary) -> String {
        switch run.outcome {
        case .completed: "Completed"
        case .aborted: "Aborted"
        case .failed: "Failed"
        case nil: "Running"
        }
    }

    private func runGlyph(_ run: RunSummary) -> HorusGlyph {
        switch run.outcome {
        case .completed: .checkCircle
        case .aborted: .stopFill
        case .failed: .xCircle
        case nil: .arrowClockwise
        }
    }

    private func runColor(_ run: RunSummary) -> Color {
        switch run.outcome {
        case .completed: palette.signal
        case .aborted: palette.warning
        case .failed: palette.danger
        case nil: palette.accent
        }
    }
}

private struct UsageHeatmap: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiatesWithoutColor
    let days: [DailyUsage]

    var body: some View {
        let chart = chartData
        let canvas = Canvas { context, size in
            let spacing: CGFloat = 3
            let cell = min(
                (size.width - spacing * CGFloat(profileUsageWeekCount - 1))
                    / CGFloat(profileUsageWeekCount),
                (size.height - spacing * 6) / 7
            )
            let width = cell * CGFloat(profileUsageWeekCount)
                + spacing * CGFloat(profileUsageWeekCount - 1)
            let originX = (size.width - width) / 2

            for index in chart.values.indices {
                let week = index % profileUsageWeekCount
                let weekday = index / profileUsageWeekCount
                let rect = CGRect(
                    x: originX + CGFloat(week) * (cell + spacing),
                    y: CGFloat(weekday) * (cell + spacing),
                    width: cell,
                    height: cell
                )
                let path = Path(roundedRect: rect, cornerRadius: min(2.5, cell * 0.3))
                context.fill(path, with: .color(heatColor(value: chart.values[index], maximum: chart.maximum)))
                if differentiatesWithoutColor, chart.values[index] > 0 {
                    context.stroke(path, with: .color(palette.canvas), lineWidth: 1)
                }
            }
        }
        GeometryReader { geometry in
            canvas.frame(width: geometry.size.width, height: 78)
        }
        .frame(height: 78)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(profileUsageWeekCount)-week token activity")
        .accessibilityValue("\(chart.activeDays) active days, \(chart.totalTokens) total tokens")
    }

    private var chartData: (values: [Int], maximum: Int, activeDays: Int, totalTokens: Int) {
        let values = days.reduce(into: [UInt64: Int]()) { values, day in
            values[day.unixDay, default: 0] += day.usage.totalTokens
        }
        let today = UInt64(Date.now.timeIntervalSince1970 / 86_400)
        let dayCount = profileUsageWeekCount * 7
        let start = today - min(today, UInt64(dayCount - 1))
        let samples = (0..<dayCount).map { index in
            let week = index % profileUsageWeekCount
            let weekday = index / profileUsageWeekCount
            return values[start + UInt64(week * 7 + weekday)] ?? 0
        }
        return (
            samples,
            max(samples.max() ?? 0, 1),
            samples.filter { $0 > 0 }.count,
            samples.reduce(0, +)
        )
    }

    private func heatColor(value: Int, maximum: Int) -> Color {
        guard value > 0 else { return palette.line.opacity(0.35) }
        let ratio = Double(value) / Double(maximum)
        return palette.accent.opacity(0.25 + 0.75 * ratio.squareRoot())
    }
}

private struct AppearanceSettings: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Picker("Theme", selection: Binding(
            get: { model.theme },
            set: { model.setTheme($0) }
        )) {
            ForEach(ThemePreference.allCases) { Text($0.rawValue.capitalized).tag($0) }
        }
        .pickerStyle(.segmented)
        .sensoryFeedback(.selection, trigger: model.theme)
    }
}

private struct AppLockSettings: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        HStack(spacing: 5) {
            Toggle(model.appLockAuthenticationMethod.settingTitle, isOn: Binding(
                get: { model.appLockEnabled },
                set: { enabled in
                    Task { await model.setAppLockEnabled(enabled) }
                }
            ))
            .toggleStyle(.switch)
            .disabled(
                model.isAppLockAuthenticating
                    || !model.appLockEnabled && !model.appLockAuthenticationMethod.isAvailable
            )
            SettingsInfoButton(
                title: model.appLockAuthenticationMethod.settingTitle,
                detail: description
            )
        }
        .onAppear { model.refreshAppLockAuthenticationMethod() }
        if model.isAppLockAuthenticating {
            ProgressView("Authenticating")
        }
        if let error = model.appLockError {
            Text(error)
                .foregroundStyle(palette.danger)
                .accessibilityLabel("App lock status: \(error)")
        }
    }

    private var description: String {
        if model.appLockAuthenticationMethod.isAvailable {
            return "Locks Horus when it enters the background. This setting stays on this device."
        }
        return "Set up Face ID or Touch ID in Settings before enabling app lock."
    }
}

private func compact(_ value: Int) -> String {
    value.formatted(.number.notation(.compactName).precision(.fractionLength(0 ... 1)))
}

private func compact(_ value: UInt64) -> String {
    value.formatted(.number.notation(.compactName).precision(.fractionLength(0 ... 1)))
}

private func formatMilliseconds(_ milliseconds: UInt64) -> String {
    let seconds = Int(clamping: milliseconds / 1_000)
    return Duration.seconds(seconds).formatted(
        .time(pattern: .minuteSecond(padMinuteToLength: 1))
    )
}
