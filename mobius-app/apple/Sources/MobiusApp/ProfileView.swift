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
    @Environment(\.mobiusPalette) private var palette
    @State private var showsInformation = false

    var body: some View {
        Button {
            showsInformation = true
        } label: {
            MobiusIcon(.info, size: MobiusStyle.glyphInline, foreground: palette.muted)
        }
        .buttonStyle(MobiusIconButtonStyle())
        .accessibilityLabel("About möbius")
        .accessibilityHint("Shows version, legal, and support information")
        .help("About möbius")
        .popover(
            isPresented: $showsInformation,
            attachmentAnchor: .rect(.bounds),
            arrowEdge: .top
        ) {
            VStack(alignment: .leading, spacing: 0) {
                Text(versionDescription)
                    .font(MobiusStyle.controlFont)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.bottom, MobiusSpace.l)
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
            .padding(MobiusSpace.l)
            .frame(width: 320, alignment: .leading)
            .presentationCompactAdaptation(.popover)
        }
    }

    private var versionDescription: String {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")
            as? String ?? "—"
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "—"
        return "möbius v\(version) (\(build))"
    }

    private func showPlaceholder(_ title: String) {
        showsInformation = false
        model.showToast("\(title) will be available before the cloud release.")
    }
}

private struct SettingsInformationRow: View {
    @Environment(\.mobiusPalette) private var palette
    let title: String
    let glyph: MobiusGlyph
    let action: @MainActor () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: MobiusSpace.m) {
                MobiusIcon(glyph, size: MobiusStyle.glyphLead, foreground: palette.muted)
                Text(title)
                Spacer(minLength: MobiusSpace.m)
                MobiusIcon(.arrowUpRight01, size: MobiusStyle.glyphMark, foreground: palette.muted)
            }
            .frame(maxWidth: .infinity, minHeight: MobiusStyle.iconButtonSize)
            .contentShape(Rectangle())
        }
        .buttonStyle(.mobiusPlain)
    }
}

/// The included-usage line. It belongs to the subscription, not to the usage dashboard,
/// which counts what this device has spent against any provider.
private struct CloudAllowanceStatus: View {
    @Environment(\.mobiusPalette) private var palette

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: MobiusSpace.m) {
            Text("Included Luna usage")
            Spacer(minLength: MobiusSpace.s)
            Text("Cloud only")
                .foregroundStyle(palette.muted)
        }
        .accessibilityElement(children: .combine)
    }
}

private struct CloudAccountSettings: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette
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
            Text("möbius works on its own with a gateway you run. Connect möbius Cloud to have one provisioned and managed for you.")
                .font(MobiusStyle.bodyFont)
                .foregroundStyle(palette.muted)
                .fixedSize(horizontal: false, vertical: true)
            MobiusCloudOfferButton()
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
    @Environment(\.mobiusPalette) private var palette

    var body: some View {
        // Cloud data rows describe data that only exists once there is a cloud account.
        if model.hasCloudAccount {
            Button("View cloud data", glyph: .hardDrives) {}
            Button("Delete cloud data", glyph: .trash, role: .destructive) {}
        }

        Toggle("Help improve möbius", isOn: Binding(
            get: { model.sharesMobiusDiagnostics },
            set: { model.setSharesMobiusDiagnostics($0) }
        ))
        .toggleStyle(.switch)

        Text("Off by default, and stored on this device.")
            .font(MobiusStyle.captionFont)
            .foregroundStyle(palette.muted)
            .fixedSize(horizontal: false, vertical: true)
    }
}

private let profileUsageWeekCount = 25

private struct ProfileUsageSection: View {
    @Environment(\.mobiusPalette) private var palette
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
        LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: MobiusSpace.s), count: 4), spacing: MobiusSpace.l) {
            UsageMetric(label: "Tokens", value: compact(total.totalTokens))
            UsageMetric(label: "Input", value: compact(total.inputTokens))
            UsageMetric(label: "Output", value: compact(total.outputTokens))
            UsageMetric(label: "Cached", value: cacheHit(total))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ProfileUsageHistory: View {
    @Environment(\.mobiusPalette) private var palette
    @State private var aggregation: UsageAggregation = .daily
    let days: [DailyUsage]
    let providerLabels: [String: String]

    var body: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.l) {
            VStack(alignment: .leading, spacing: MobiusSpace.l) {
                Text("Token activity")
                    .font(MobiusStyle.controlFont)
                Picker("Usage grouping", selection: $aggregation) {
                    ForEach(UsageAggregation.allCases) { option in
                        Text(option.title).tag(option)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .sensoryFeedback(.selection, trigger: aggregation)
                UsageHeatmap(days: days, aggregation: aggregation)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(MobiusStyle.cardPadding)
            .background(palette.panel, in: MobiusStyle.cardShape)
            .overlay {
                MobiusStyle.cardShape.stroke(
                    palette.line.opacity(0.45),
                    lineWidth: MobiusStyle.borderWidth
                )
                .allowsHitTesting(false)
            }
            HStack(alignment: .firstTextBaseline) {
                Text("By provider")
                    .font(MobiusStyle.controlFont)
                Spacer()
                Text("Last \(profileUsageWeekCount) weeks")
                    .font(MobiusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
            }
            ProviderUsageChart(
                usage: days,
                providerLabels: providerLabels,
                weekCount: profileUsageWeekCount,
                aggregation: aggregation
            )
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct UsageMetric: View {
    @Environment(\.mobiusPalette) private var palette
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.xs) {
            // Sentence case, not tracked-out monospace caps: the value is the number, and a
            // shouted label competes with it at the same size the number is set in.
            Text(label)
                .font(MobiusStyle.captionFont)
                .foregroundStyle(palette.muted)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
            Text(value)
                .font(MobiusStyle.titleFont)
                .monospacedDigit()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ProfileRunStatsSection: View {
    let stats: RunStats

    var body: some View {
        VStack(spacing: MobiusSpace.l) {
            HStack(spacing: MobiusSpace.s) {
                UsageMetric(label: "Runs", value: compact(stats.runCount))
                UsageMetric(label: "Failed", value: compact(stats.failedRunCount))
                UsageMetric(label: "Aborted", value: compact(stats.abortedRunCount))
                UsageMetric(label: "Elapsed", value: formatMilliseconds(stats.elapsedMs))
            }
            HStack(spacing: MobiusSpace.s) {
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
    @Environment(\.mobiusPalette) private var palette
    @State private var collapsedGroupIDs: Set<String> = []
    let groups: [SessionRunGroup]

    var body: some View {
        if groups.isEmpty {
            Text("No completed runs yet.")
                .font(MobiusStyle.bodyFont)
                .foregroundStyle(palette.muted)
        } else {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(groups) { group in
                        DisclosureGroup(isExpanded: expansion(for: group.id)) {
                            ForEach(group.runs) { run in
                                Button {
                                    model.openChat(group.sessionId)
                                } label: {
                                    HStack(spacing: MobiusSpace.m) {
                                        MobiusIcon(runGlyph(run), foreground: runColor(run))
                                        VStack(alignment: .leading, spacing: MobiusSpace.xxs) {
                                            HStack(spacing: MobiusSpace.s) {
                                                Text(run.sessionId == group.sessionId ? "Run" : "Sub-run")
                                                    .font(MobiusStyle.metadataFont.weight(.semibold))
                                                Text(
                                                    runDate(run),
                                                    format: .dateTime.month(.abbreviated).day().hour().minute()
                                                )
                                                .font(MobiusStyle.metadataFont)
                                                .foregroundStyle(palette.muted)
                                            }
                                            Text(runDetail(run))
                                                .font(MobiusStyle.metadataFont)
                                                .foregroundStyle(palette.muted)
                                                .lineLimit(1)
                                        }
                                        Spacer(minLength: MobiusSpace.xs)
                                        MobiusIcon(.caretRight, size: MobiusStyle.glyphMark, foreground: palette.muted)
                                    }
                                    .frame(maxWidth: .infinity, minHeight: MobiusStyle.iconButtonSize)
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.mobiusPlain)
                                .disabled(
                                    !model.canOpenSession
                                        && group.sessionId != model.selectedSessionID
                                )
                                .accessibilityLabel("\(runOutcome(run)), \(group.title)")
                                .accessibilityValue(runDetail(run))
                                .accessibilityHint("Opens the chat for this run")
                            }
                        } label: {
                            HStack(spacing: MobiusSpace.s) {
                                Text(group.title)
                                    .font(MobiusStyle.controlFont)
                                    .lineLimit(1)
                                Text(group.runs.count, format: .number)
                                    .font(MobiusStyle.metadataFont)
                                    .foregroundStyle(palette.muted)
                            }
                            .frame(maxWidth: .infinity, minHeight: MobiusStyle.iconButtonSize)
                        }
                        .tint(palette.accent)
                    }
                }
            }
            .frame(height: CGFloat(min(visibleRowCount, 20)) * MobiusStyle.iconButtonSize)
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

    private func runGlyph(_ run: RunSummary) -> MobiusGlyph {
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
    @Environment(\.mobiusPalette) private var palette
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiatesWithoutColor
    let days: [DailyUsage]
    let aggregation: UsageAggregation

    var body: some View {
        let chart = chartData
        VStack(alignment: .leading, spacing: MobiusSpace.xs) {
            Canvas { context, size in
                let gapRatio: CGFloat = 0.28
                let cell = size.width / (
                    CGFloat(profileUsageWeekCount)
                        + gapRatio * CGFloat(profileUsageWeekCount - 1)
                )
                let spacing = cell * gapRatio

                for index in chart.values.indices {
                    let week = index / 7
                    let weekday = index % 7
                    guard week < profileUsageWeekCount else { continue }
                    let value = chart.values[index]
                    let rect = CGRect(
                        x: CGFloat(week) * (cell + spacing),
                        y: CGFloat(weekday) * (cell + spacing),
                        width: cell,
                        height: cell
                    )
                    let path = Path(roundedRect: rect, cornerRadius: min(4, cell * 0.3))
                    context.fill(path, with: .color(heatColor(value: value, maximum: chart.maximum)))
                    if differentiatesWithoutColor, value > 0 {
                        context.stroke(path, with: .color(palette.canvas), lineWidth: 1)
                    }
                }
            }
            .aspectRatio(heatmapAspectRatio, contentMode: .fit)

            GeometryReader { geometry in
                let gapRatio: CGFloat = 0.28
                let cell = geometry.size.width / (
                    CGFloat(profileUsageWeekCount)
                        + gapRatio * CGFloat(profileUsageWeekCount - 1)
                )
                let spacing = cell * gapRatio
                ZStack(alignment: .topLeading) {
                    ForEach(monthLabels) { label in
                        Text(label.title)
                            .font(MobiusStyle.metadataFont)
                            .foregroundStyle(palette.muted)
                            .position(
                                x: CGFloat(label.week) * (cell + spacing) + 12,
                                y: 8
                            )
                    }
                }
            }
            .frame(height: 16)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(aggregation.title) token activity")
        .accessibilityValue("\(chart.activeDays) active days, \(chart.totalTokens) total tokens")
    }

    private var heatmapAspectRatio: CGFloat {
        let gapRatio: CGFloat = 0.28
        return (
            CGFloat(profileUsageWeekCount)
                + gapRatio * CGFloat(profileUsageWeekCount - 1)
        ) / (7 + gapRatio * 6)
    }

    private var monthLabels: [UsageMonthLabel] {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        let today = UInt64(Date.now.timeIntervalSince1970 / 86_400)
        let dayCount = profileUsageWeekCount * 7
        let start = today - min(today, UInt64(dayCount - 1))
        var labels: [UsageMonthLabel] = []
        var previousMonth: Int?
        for week in 0..<profileUsageWeekCount {
            let date = Date(timeIntervalSince1970: TimeInterval(start + UInt64(week * 7)) * 86_400)
            let month = calendar.component(.month, from: date)
            guard month != previousMonth else { continue }
            previousMonth = month
            labels.append(UsageMonthLabel(
                week: week,
                title: date.formatted(.dateTime.month(.narrow))
            ))
        }
        return labels
    }

    private var chartData: UsageActivitySnapshot {
        UsageActivitySeries.snapshot(
            from: days,
            endingOn: UInt64(Date.now.timeIntervalSince1970 / 86_400),
            weekCount: profileUsageWeekCount,
            aggregation: aggregation
        )
    }

    private func heatColor(value: Int, maximum: Int) -> Color {
        guard value > 0 else { return palette.line.opacity(0.35) }
        let ratio = Double(value) / Double(maximum)
        return palette.accent.opacity(0.25 + 0.75 * ratio.squareRoot())
    }
}

private struct UsageMonthLabel: Identifiable {
    let week: Int
    let title: String

    var id: Int { week }
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
    @Environment(\.mobiusPalette) private var palette

    var body: some View {
        HStack(spacing: MobiusSpace.xs) {
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
            return "Locks möbius when it enters the background. This setting stays on this device."
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
