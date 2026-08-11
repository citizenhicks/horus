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
            // Settings first, the dashboard last: this page is opened to change something,
            // and usage is the one section here nobody comes to act on.
            Section("Account") {
                CloudAccountSettings()
            }
            .listRowSeparator(.hidden)
            Section("Appearance") {
                AppearanceSettings()
            }
            .listRowSeparator(.hidden)
            Section("Security") {
                AppLockSettings()
            }
            .listRowSeparator(.hidden)
            Section("Data & Privacy") {
                DataPrivacySettings()
            }
            .listRowSeparator(.hidden)
            Section("Usage") {
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
            .listRowSeparator(.hidden)
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
            // Bare, like the agent status dot and the per-setting info buttons: a toolbar
            // accessory in this app carries no chrome, and a glass circle on this one alone
            // read as a different kind of control.
            HorusIcon(.info, size: HorusStyle.glyphInline, foreground: palette.muted)
                .frame(
                    minWidth: HorusStyle.iconButtonSize,
                    minHeight: HorusStyle.iconButtonSize
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
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
                    .padding(.bottom, HorusSpace.l)
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
            .padding(HorusSpace.l)
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
            HStack(spacing: HorusSpace.m) {
                HorusIcon(glyph, size: HorusStyle.glyphLead, foreground: palette.muted)
                Text(title)
                Spacer(minLength: HorusSpace.m)
                HorusIcon(.arrowUpRight01, size: HorusStyle.glyphMark, foreground: palette.muted)
            }
            .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize)
            .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
    }
}

/// The included-usage line. It belongs to the subscription, not to the usage dashboard,
/// which counts what this device has spent against any provider.
private struct CloudAllowanceStatus: View {
    @Environment(\.horusPalette) private var palette

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: HorusSpace.m) {
            Text("Included Luna usage")
            Spacer(minLength: HorusSpace.s)
            Text("Cloud only")
                .foregroundStyle(palette.muted)
        }
        .accessibilityElement(children: .combine)
    }
}

private struct CloudAccountSettings: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var isRestoringPurchases = false
    @State private var showsSubscriptionManagement = false

    /// Signed out, this is an offer; signed in, it is an account. The rows that only make
    /// sense with an account are absent rather than greyed out — the one exception is
    /// Restore Purchases, which has to stay reachable whether or not a purchase is known.
    var body: some View {
        if model.hasCloudAccount {
            CloudAllowanceStatus()
            Button("Manage subscription", glyph: .sealCheck) {
                showsSubscriptionManagement = true
            }
            .accessibilityHint("Opens App Store subscription management, where you can unsubscribe")
            .manageSubscriptionsSheet(isPresented: $showsSubscriptionManagement)
            Button("Delete account", glyph: .trash, role: .destructive) {}
        } else {
            Text("Horus works on its own with a gateway you run. Connect Horus Cloud to have one provisioned and managed for you.")
                .font(HorusStyle.bodyFont)
                .foregroundStyle(palette.muted)
                .fixedSize(horizontal: false, vertical: true)
            HorusCloudOfferButton()
        }

        Button(
            isRestoringPurchases ? "Restoring purchases…" : "Restore purchases",
            glyph: .arrowClockwise,
            action: restorePurchases
        )
        .disabled(isRestoringPurchases)
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
        // Cloud data rows describe data that only exists once there is a cloud account.
        if model.hasCloudAccount {
            Button("View cloud data", glyph: .hardDrives) {}
            Button("Delete cloud data", glyph: .trash, role: .destructive) {}
        }

        Toggle("Help improve Horus", isOn: Binding(
            get: { model.sharesHorusDiagnostics },
            set: { model.setSharesHorusDiagnostics($0) }
        ))
        .toggleStyle(.switch)

        Text("Off by default, and stored on this device.")
            .font(HorusStyle.captionFont)
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
        // Four fixed columns: an adaptive grid drops to three and orphans the last metric.
        // The section header already says "Usage", so the grid needs no heading of its own.
        LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: HorusSpace.s), count: 4), spacing: HorusSpace.l) {
            UsageMetric(label: "Tokens", value: compact(total.totalTokens))
            UsageMetric(label: "Input", value: compact(total.inputTokens))
            UsageMetric(label: "Output", value: compact(total.outputTokens))
            UsageMetric(label: "Cached", value: cacheHit(total))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ProfileUsageHistory: View {
    @Environment(\.horusPalette) private var palette
    let days: [DailyUsage]
    let providerLabels: [String: String]

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.l) {
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
        VStack(alignment: .leading, spacing: HorusSpace.xs) {
            // Sentence case, not tracked-out monospace caps: the value is the number, and a
            // shouted label competes with it at the same size the number is set in.
            Text(label)
                .font(HorusStyle.captionFont)
                .foregroundStyle(palette.muted)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
            Text(value)
                .font(HorusStyle.titleFont)
                .monospacedDigit()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ProfileRunStatsSection: View {
    let stats: RunStats

    var body: some View {
        VStack(spacing: HorusSpace.l) {
            HStack(spacing: HorusSpace.s) {
                UsageMetric(label: "Runs", value: compact(stats.runCount))
                UsageMetric(label: "Failed", value: compact(stats.failedRunCount))
                UsageMetric(label: "Aborted", value: compact(stats.abortedRunCount))
                UsageMetric(label: "Elapsed", value: formatMilliseconds(stats.elapsedMs))
            }
            HStack(spacing: HorusSpace.s) {
                UsageMetric(label: "Model calls", value: compact(stats.modelCalls))
                UsageMetric(label: "Tool calls", value: compact(stats.toolCalls))
                UsageMetric(label: "Tool errors", value: compact(stats.failedToolCalls))
                UsageMetric(label: "Run tokens", value: compact(stats.usage.totalTokens))
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
                                    HStack(spacing: HorusSpace.m) {
                                        HorusIcon(runGlyph(run), foreground: runColor(run))
                                        VStack(alignment: .leading, spacing: HorusSpace.xxs) {
                                            HStack(spacing: HorusSpace.s) {
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
                                        Spacer(minLength: HorusSpace.xs)
                                        HorusIcon(.caretRight, size: HorusStyle.glyphMark, foreground: palette.muted)
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
                            HStack(spacing: HorusSpace.s) {
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
        HStack(spacing: HorusSpace.xs) {
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
