import SwiftUI

struct AgentSettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        PageScaffold(
            title: "Agent",
            detail: "Change the prompt, capabilities, and execution policy used by the gateway agent."
        ) {
            if model.agentDraft != nil {
                applyStatus.settingsStandaloneRow()
                Section("System prompt") {
                    TextField("System prompt", text: systemPrompt, axis: .vertical)
                        .font(HorusStyle.bodyFont)
                        .lineLimit(2...)
                        .textFieldStyle(.plain)
                        .labelsHidden()
                        .accessibilityLabel("System prompt")
                }

                Section("Capabilities") {
                    capabilityToggle("Workspace tools", detail: "Read, edit, and run commands", binding: middleware(\.tools))
                    capabilityToggle("Skills", detail: "Discover gateway-installed instructions", binding: middleware(\.skills))
                    capabilityToggle("Subagents", detail: "Delegate bounded concurrent work", binding: middleware(\.subagents))
                    capabilityToggle("Steering", detail: "Accept input while a turn is active", binding: middleware(\.steering))
                    capabilityToggle("Compaction", detail: "Manage long context automatically", binding: middleware(\.compaction))
                    capabilityToggle("Sessions", detail: "Resume and fork durable chats", binding: middleware(\.sessions))
                }
                .toggleStyle(.switch)

                Section("Execution approval") {
                    LabeledContent {
                        Picker("Approval policy", selection: approvalPolicy) {
                            Text("Ask").tag(ApprovalPolicy.on)
                            Text("Allow · no network").tag(ApprovalPolicy.allow)
                            Text("Allow · network").tag(ApprovalPolicy.allowNetwork)
                        }
                        .labelsHidden()
                        .pickerStyle(.menu)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Approval policy").font(HorusStyle.controlFont)
                            if let approvalDescription {
                                Text(approvalDescription)
                                    .font(HorusStyle.bodyFont)
                                    .foregroundStyle(palette.muted)
                            }
                        }
                    }
                    .frame(maxWidth: .infinity)
                    if model.agentDraft?.approval == .allowNetwork {
                        HorusLabel(
                            title: "This permits unprompted network-capable tools. Only use it with a gateway and workspace you trust.",
                            icon: "globe-lock",
                            iconColor: palette.danger
                        )
                        .font(HorusStyle.controlFont)
                        .foregroundStyle(palette.danger)
                    }
                }

                Button(action: model.applyAgentConfiguration) {
                    Text("Apply and restart agent")
                        .frame(maxWidth: settingsActionMaxWidth)
                }
                .settingsPrimaryAction()
                .disabled(!hasChanges || model.applyState == .applying || model.applyState == .restarting)
                .settingsStandaloneRow()
            } else {
                ContentUnavailableView {
                    Label {
                        Text("Agent unavailable")
                    } icon: {
                        HorusIcon(name: "toggle-left", size: 32)
                    }
                } description: {
                    Text("Connect to a gateway first.")
                }
            }
        }
    }

    @ViewBuilder
    private var applyStatus: some View {
        switch model.applyState {
        case .idle, .applied:
            if !hasChanges {
                HStack(spacing: 7) {
                    Circle()
                        .fill(palette.signal)
                        .frame(width: 7, height: 7)
                        .accessibilityHidden(true)
                    Text("Up to date")
                        .font(HorusStyle.controlFont)
                        .foregroundStyle(palette.signal)
                }
                .padding(.horizontal, 12)
                .frame(height: 32)
                .horusGlass(in: Capsule())
                .frame(maxWidth: .infinity, alignment: .leading)
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
                .accessibilityElement(children: .combine)
                .accessibilityLabel("Agent is up to date")
            }
        case .applying:
            StatusBanner(tone: .neutral, title: "Applying configuration", detail: "The gateway is validating this revision.", progress: true)
        case .restarting:
            StatusBanner(tone: .warning, title: "Restarting agent", detail: "The gateway accepted the configuration and is reopening the session.", progress: true)
        case .busy(let message):
            StatusBanner(tone: .warning, title: "Agent is busy", detail: message)
        case .conflict(let message):
            StatusBanner(tone: .warning, title: "Configuration changed elsewhere", detail: message, action: ("Reload", model.reloadAgentDraft))
        case .invalid(let message):
            StatusBanner(tone: .error, title: "Configuration rejected", detail: message)
        case .failed(let message):
            StatusBanner(tone: .error, title: "Could not apply", detail: message)
        }
    }

    private func capabilityToggle(_ title: String, detail: String, binding: Binding<Bool>) -> some View {
        LabeledContent {
            Toggle(title, isOn: binding)
                .labelsHidden()
        } label: {
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(HorusStyle.controlFont)
                Text(detail).font(HorusStyle.bodyFont).foregroundStyle(palette.muted)
            }
        }
        .frame(maxWidth: .infinity)
    }

    private var systemPrompt: Binding<String> {
        Binding(
            get: { model.agentDraft?.systemPrompt ?? "" },
            set: { model.agentDraft?.systemPrompt = $0 }
        )
    }

    private func middleware(_ keyPath: WritableKeyPath<MiddlewareSelection, Bool>) -> Binding<Bool> {
        Binding(
            get: { model.agentDraft?.middleware[keyPath: keyPath] ?? false },
            set: { model.agentDraft?.middleware[keyPath: keyPath] = $0 }
        )
    }

    private var approvalPolicy: Binding<ApprovalPolicy> {
        Binding(
            get: { model.agentDraft?.approval ?? .on },
            set: { model.agentDraft?.approval = $0 }
        )
    }

    private var approvalDescription: String? {
        switch model.agentDraft?.approval ?? .on {
        case .on: "Workspace mutations pause in chat for an explicit decision."
        case .allow: "Workspace mutations can proceed without prompting, but tools receive no network access."
        case .allowNetwork: nil
        }
    }

    private var hasChanges: Bool {
        guard let snapshot = model.agentSnapshot, let draft = model.agentDraft else { return false }
        return snapshot.config != draft
    }
}

struct ProvidersView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        PageScaffold(
            title: "Providers",
            detail: "Provider state belongs to the gateway. Credential material is write-only and is never returned here."
        ) {
            if model.agentDraft != nil {
                Section("Configured") {
                    if configuredProviders.isEmpty {
                        Text("No provider configured on this gateway.")
                            .foregroundStyle(palette.muted)
                    } else {
                        ForEach(configuredProviders) { status in
                            LabeledContent(status.label) {
                                Text("Configured")
                                    .foregroundStyle(palette.signal)
                            }
                        }
                    }
                }

                Section("New configuration") {
                    LabeledContent("Provider") {
                        Picker("Provider", selection: providerID) {
                            ForEach(model.providerStatuses) { status in
                                Text(status.label).tag(status.provider)
                            }
                        }
                        .labelsHidden()
                        .pickerStyle(.menu)
                    }
                    .frame(maxWidth: .infinity)

                    #if os(iOS)
                    VStack(alignment: .leading, spacing: 7) {
                        Text("Base URL override")
                            .font(HorusStyle.controlFont)
                        TextField("Use provider default", text: providerBaseURL)
                            .textFieldStyle(.roundedBorder)
                            .textContentType(.URL)
                    }
                    #else
                    LabeledContent("Base URL override") {
                        TextField("Use provider default", text: providerBaseURL)
                            .textFieldStyle(.roundedBorder)
                            .textContentType(.URL)
                            .frame(maxWidth: 300)
                    }
                    #endif
                    LabeledContent("Hosted web search") {
                        Picker("Hosted web search", selection: providerWebSearch) {
                            Text("Off").tag("off")
                            Text("Cached").tag("cached")
                            Text("Live").tag("live")
                        }
                        .labelsHidden()
                        .pickerStyle(.menu)
                    }
                    .frame(maxWidth: .infinity)
                }

                if selectedStatus != nil {
                    Section("Credential") {
                        credentialControls
                    }
                }

                providerActionStatus

                Button(action: model.applyAgentConfiguration) {
                    Text("Apply provider and restart")
                        .frame(maxWidth: settingsActionMaxWidth)
                }
                .settingsPrimaryAction()
                .disabled(
                    model.agentDraft == model.agentSnapshot?.config
                        || !providerConfigurationValid
                        || model.applyState == .applying
                        || model.applyState == .restarting
                )
                .settingsStandaloneRow()
            } else {
                ContentUnavailableView {
                    Label {
                        Text("Providers unavailable")
                    } icon: {
                        HorusIcon(name: "cpu", size: 32)
                    }
                } description: {
                    Text("Connect to a gateway first.")
                }
            }
        }
    }

    @ViewBuilder
    private var credentialControls: some View {
        if let status = selectedStatus {
            LabeledContent("Status") {
                Text(status.configured ? "Configured on gateway" : "Not configured")
                    .font(HorusStyle.controlFont)
                    .foregroundStyle(status.configured ? palette.signal : palette.warning)
            }

            if status.auth == "api_key" {
                HStack {
                    SecureField("New API key · write only", text: Binding(
                        get: { model.providerAPIKey },
                        set: { model.providerAPIKey = $0 }
                    ))
                    .textFieldStyle(.roundedBorder)
                    .textContentType(.password)
                    Button(
                        "Send key to gateway",
                        lucideIcon: "key-round",
                        action: { model.saveProviderCredential(provider: status.provider) }
                    )
                    .labelStyle(.iconOnly)
                    .buttonStyle(HorusIconButtonStyle(prominent: true))
                    .help("Send key to gateway")
                    .disabled(model.providerAPIKey.isEmpty)
                }
            } else if status.auth == "device_code" {
                Button(action: { model.startProviderLogin(provider: status.provider) }) {
                    Text("Start device sign-in")
                        .frame(maxWidth: settingsActionMaxWidth)
                }
                .buttonStyle(.glassProminent)
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .frame(maxWidth: .infinity)
            }
        }
    }

    @ViewBuilder
    private var providerActionStatus: some View {
        switch model.providerActionState {
        case .idle:
            EmptyView()
        case .savingCredential:
            StatusBanner(tone: .neutral, title: "Sending credential", detail: "The value is not persisted by this app.", progress: true)
        case .credentialSaved(let provider):
            StatusBanner(tone: .success, title: "Credential updated", detail: "\(provider) is configured on the gateway.")
        case .startingLogin(let provider):
            StatusBanner(tone: .neutral, title: "Starting \(provider) sign-in", detail: "Waiting for a device code.", progress: true)
        case .deviceCode(let provider, let url, let code):
            VStack(alignment: .leading, spacing: 12) {
                Text("Finish \(provider) sign-in")
                    .font(.headline)
                Text("Open the verification page and enter this code.")
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                Text(code)
                    .font(.system(.title, design: .monospaced, weight: .bold))
                    .tracking(3)
                    .textSelection(.enabled)
                    .padding(12)
                    .background(palette.raised, in: RoundedRectangle(cornerRadius: HorusStyle.controlRadius))
                ViewThatFits(in: .horizontal) {
                    HStack {
                        if let destination = URL(string: url) {
                            Link("Open verification page", destination: destination)
                                .buttonStyle(.glass)
                                .buttonBorderShape(.capsule)
                                .controlSize(.large)
                        }
                        ShareLink("Copy or share code", item: code)
                            .buttonStyle(.glass)
                            .buttonBorderShape(.capsule)
                            .controlSize(.large)
                    }
                    VStack(alignment: .leading) {
                        if let destination = URL(string: url) {
                            Link("Open verification page", destination: destination)
                                .buttonStyle(.glass)
                                .buttonBorderShape(.capsule)
                                .controlSize(.large)
                        }
                        ShareLink("Copy or share code", item: code)
                            .buttonStyle(.glass)
                            .buttonBorderShape(.capsule)
                            .controlSize(.large)
                    }
                }
            }
        case .loginFinished(let provider):
            StatusBanner(tone: .success, title: "Sign-in complete", detail: "\(provider) is ready on the gateway.")
        case .failed(let message):
            StatusBanner(tone: .error, title: "Provider action failed", detail: message)
        }
    }

    private var selectedStatus: ProviderStatus? {
        guard let provider = model.agentDraft?.provider.provider else { return nil }
        return model.providerStatuses.first { $0.provider == provider }
    }

    private var configuredProviders: [ProviderStatus] {
        model.providerStatuses.filter(\.configured)
    }

    private var providerID: Binding<String> {
        Binding(
            get: { model.agentDraft?.provider.provider ?? "" },
            set: { model.selectProvider($0) }
        )
    }

    private var providerWebSearch: Binding<String> {
        Binding(
            get: { model.agentDraft?.provider.webSearch ?? "off" },
            set: { model.agentDraft?.provider.webSearch = $0 }
        )
    }

    private var providerBaseURL: Binding<String> {
        Binding(
            get: { model.agentDraft?.provider.baseUrl ?? "" },
            set: { model.agentDraft?.provider.baseUrl = $0.nonEmpty }
        )
    }

    private var providerConfigurationValid: Bool {
        guard let provider = model.agentDraft?.provider else { return false }
        return !provider.provider.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !provider.model.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

struct CronView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        PageScaffold(
            title: "Schedules",
            detail: "Run durable Horus tasks on the gateway workspace, even when this app is closed."
        ) {
            Section("Add schedule") {
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Task file")
                            .font(HorusStyle.controlFont)
                        Text(model.cronTaskDraft.isEmpty ? "No file selected" : model.cronTaskDraft)
                            .font(HorusStyle.bodyFont)
                            .foregroundStyle(palette.muted)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: 12)
                    Button("Choose", lucideIcon: "folder-open", action: model.openCronTaskBrowser)
                        .buttonStyle(.glass)
                        .buttonBorderShape(.capsule)
                        .controlSize(.large)
                }
                TextField("Cron expression · e.g. 15 2 * * 1-5", text: Binding(
                    get: { model.cronScheduleDraft }, set: { model.cronScheduleDraft = $0 }
                ))
                .textFieldStyle(.roundedBorder)
                .font(HorusStyle.bodyFont.monospaced())
            }

            Button(action: model.addCron) {
                HorusLabel(title: "Add schedule", icon: "plus")
                    .frame(maxWidth: settingsActionMaxWidth)
            }
            .settingsPrimaryAction()
            .settingsStandaloneRow()

            if let error = model.cronError {
                StatusBanner(tone: .error, title: "Schedule rejected", detail: error)
            }

            Section {
                if model.cronTasks.isEmpty {
                    Text("No scheduled tasks yet.").foregroundStyle(palette.muted)
                }
                ForEach(model.cronTasks) { task in
                    CronTaskRow(task: task)
                }
            } header: {
                HStack {
                    Text("Tasks")
                    Spacer()
                    Button("Refresh", lucideIcon: "refresh-cw") { model.refreshCron() }
                        .labelStyle(.iconOnly)
                        .buttonStyle(HorusIconButtonStyle())
                        .help("Refresh schedules")
                }
            }

            Section("Run history") {
                if model.cronRuns.isEmpty {
                    Text("No scheduled runs yet.").foregroundStyle(palette.muted)
                }
                ForEach(model.cronRuns) { run in
                    CronRunRow(run: run)
                }
            }
        }
        .task { if model.connectionState.isReady { model.refreshCron() } }
    }
}

private struct CronTaskRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let task: CronTask
    @State private var schedule: String

    init(task: CronTask) {
        self.task = task
        _schedule = State(initialValue: task.schedule)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 5) {
                    Text(task.task)
                        .font(HorusStyle.metadataFont.weight(.semibold))
                        .textSelection(.enabled)
                    Text("ID \(task.id)")
                        .font(HorusStyle.metadataFont)
                        .foregroundStyle(palette.muted)
                }
                Spacer()
                Button("Run now", lucideIcon: "play") { model.runCron(task) }
                    .buttonStyle(.glassProminent)
                    .buttonBorderShape(.capsule)
                    .controlSize(.large)
            }
            HStack {
                TextField("Schedule", text: $schedule)
                    .textFieldStyle(.roundedBorder)
                    .font(HorusStyle.bodyFont.monospaced())
                Button("Reschedule") { model.rescheduleCron(task, schedule: schedule) }
                    .buttonStyle(.glass)
                    .buttonBorderShape(.capsule)
                    .controlSize(.large)
                    .disabled(schedule == task.schedule)
                Button("Delete schedule", lucideIcon: "trash-2", role: .destructive) {
                    model.deleteCron(task)
                }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle())
                .accessibilityLabel("Delete schedule")
            }
        }
        .onChange(of: task.schedule) { schedule = task.schedule }
    }
}

private struct CronRunRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let run: CronRun

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Circle().fill(statusColor).frame(width: 9, height: 9).padding(.top, 5)
            VStack(alignment: .leading, spacing: 5) {
                HStack {
                    Text(run.status.uppercased()).font(HorusStyle.metadataFont.weight(.bold))
                    Text(Date(timeIntervalSince1970: TimeInterval(run.startedAt)), style: .relative)
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                }
                Text("Task \(run.taskId)").font(HorusStyle.metadataFont)
                if let message = run.message {
                    Text(message).font(HorusStyle.bodyFont).foregroundStyle(palette.muted)
                }
            }
            Spacer()
            if let sessionID = run.sessionId {
                Button("Open session") {
                    model.openSession(sessionID)
                    model.destination = .chat
                }
                .buttonStyle(.glass)
                .buttonBorderShape(.capsule)
                .controlSize(.large)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(run.status) run for task \(run.taskId)")
    }

    private var statusColor: Color {
        switch run.status {
        case "succeeded": palette.signal
        case "failed": palette.danger
        case "running": palette.accent
        default: palette.muted
        }
    }
}

struct ProfileView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let usage = model.profile?.dailyUsage ?? []
        PageScaffold(
            title: "Settings",
            detail: "Token activity reported by the selected gateway."
        ) {
            Section("Usage") {
                ProfileUsageSection(days: usage)
            }
            Section("Appearance") {
                AppearanceSettings()
            }
        }
    }
}

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
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 100), spacing: 16)], spacing: 16) {
                UsageMetric(label: "TOKENS", value: compact(total.totalTokens))
                UsageMetric(label: "INPUT", value: compact(total.inputTokens))
                UsageMetric(label: "OUTPUT", value: compact(total.outputTokens))
                UsageMetric(label: "CACHE HIT", value: cacheHit(total))
            }
            Text("52-week activity")
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
                .font(.headline.weight(.semibold))
                .monospacedDigit()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct UsageHeatmap: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiatesWithoutColor
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    let days: [DailyUsage]

    var body: some View {
        let chart = chartData
        let canvas = Canvas { context, size in
            let spacing: CGFloat = 3
            let cell = min(
                (size.width - spacing * 51) / 52,
                (size.height - spacing * 6) / 7
            )
            let width = cell * 52 + spacing * 51
            let originX = (size.width - width) / 2

            for index in chart.values.indices {
                let week = index % 52
                let weekday = index / 52
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
            if horizontalSizeClass == .compact {
                ScrollView(.horizontal) {
                    canvas.frame(width: max(geometry.size.width, 620), height: 78)
                }
                .scrollIndicators(.hidden)
            } else {
                canvas.frame(width: geometry.size.width, height: 78)
            }
        }
        .frame(height: 78)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("52-week token activity")
        .accessibilityValue("\(chart.activeDays) active days, \(chart.totalTokens) total tokens")
    }

    private var chartData: (values: [Int], maximum: Int, activeDays: Int, totalTokens: Int) {
        let values = days.reduce(into: [UInt64: Int]()) { values, day in
            values[day.unixDay, default: 0] += day.usage.totalTokens
        }
        let today = UInt64(Date.now.timeIntervalSince1970 / 86_400)
        let start = today > 363 ? today - 363 : 0
        let samples = (0..<(52 * 7)).map { index in
            let week = index % 52
            let weekday = index / 52
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
    }
}

struct GatewayView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var confirmsForget = false

    var body: some View {
        PageScaffold(
            title: "Gateway",
            detail: "Manage the selected gateway and pair another device."
        ) {
            Section("Connection") {
                LabeledContent("Gateway") {
                    Picker("Gateway", selection: Binding(
                        get: { model.selectedAccountID },
                        set: { model.selectAccount($0) }
                    )) {
                        ForEach(model.accounts) { account in
                            Text(account.displayName).tag(Optional(account.id))
                        }
                    }
                    .labelsHidden()
                }
                LabeledContent("Status") {
                    HStack(spacing: 7) {
                        Circle()
                            .fill(model.connectionState.isReady ? palette.signal : palette.danger)
                            .frame(width: 7, height: 7)
                        Text(model.connectionState.label)
                    }
                    .font(HorusStyle.controlFont)
                }
                LabeledContent("Endpoint", value: model.selectedAccount?.endpoint.rawValue ?? "—")
                LabeledContent("Transport", value: model.selectedAccount?.endpoint.usesTLS == true ? "TLS" : "Loopback TCP")
                LabeledContent("Protocol", value: "Horus gateway v1")
            }

            GlassEffectContainer(spacing: 8) {
                HStack(spacing: 8) {
                    Button("Reconnect", lucideIcon: "refresh-cw", action: model.reconnect)
                        .help("Reconnect")
                    Button("Add gateway", lucideIcon: "plus") { model.showsPairing = true }
                        .help("Add gateway")
                    Button("Forget gateway", lucideIcon: "trash-2", role: .destructive) {
                        confirmsForget = true
                    }
                    .help("Forget gateway")
                }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle())
            }
            .settingsStandaloneRow()

            Section("Pair another device") {
                Text("Ask this gateway for a short-lived code, then enter it with the same gateway address on the other device.")
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                if let pairing = model.pairingCodeInfo {
                    Text(pairing.code)
                        .font(.system(.title, design: .monospaced, weight: .bold))
                        .tracking(3)
                        .textSelection(.enabled)
                    HStack {
                        HStack(spacing: 3) {
                            Text("Expires")
                            Text(pairing.expiresAt, style: .relative)
                        }
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                    }
                }
            }

            if let pairing = model.pairingCodeInfo {
                ShareLink(item: pairing.code) {
                    HorusLabel(title: "Copy or share", icon: "share")
                        .frame(maxWidth: settingsActionMaxWidth)
                }
                .buttonStyle(.glass)
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .settingsStandaloneRow()
            } else {
                Button(action: model.createPairingCode) {
                    Text("Create one-time code")
                        .frame(maxWidth: settingsActionMaxWidth)
                }
                .settingsPrimaryAction()
                .settingsStandaloneRow()
            }
        }
        .confirmationDialog(
            "Forget this gateway?",
            isPresented: $confirmsForget,
            titleVisibility: .visible
        ) {
            Button("Forget gateway", role: .destructive, action: model.forgetSelectedGateway)
        } message: {
            Text("You will need to pair with this gateway again.")
        }
    }
}

struct PageScaffold<Content: View>: View {
    @Environment(\.horusPalette) private var palette
    let title: String
    let detail: String
    let content: Content

    init(
        title: String,
        detail: String,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.detail = detail
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            #if os(macOS)
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.title2.weight(.semibold))
                Text(detail)
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
            }
            .padding(.horizontal, 38)
            .padding(.top, 20)
            .padding(.bottom, 8)
            #endif

            Form {
                #if os(iOS)
                Text(detail)
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                    .listRowBackground(Color.clear)
                #endif

                content
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        #if os(macOS)
        .frame(maxWidth: 780)
        .frame(maxWidth: .infinity)
        #else
        .navigationTitle(title)
        .toolbarTitleDisplayMode(.inline)
        #endif
        .background(HorusBackdrop())
    }
}

#if os(iOS)
private let settingsActionMaxWidth: CGFloat? = .infinity
#else
private let settingsActionMaxWidth: CGFloat? = nil
#endif

private extension View {
    func settingsPrimaryAction() -> some View {
        buttonStyle(.glassProminent)
            .buttonBorderShape(.capsule)
            .controlSize(.large)
    }

    func settingsStandaloneRow() -> some View {
        Section {
        } header: {
            self
                .frame(maxWidth: .infinity, alignment: .center)
                .textCase(nil)
        }
    }
}

private struct StatusBanner: View {
    enum Tone { case neutral, success, warning, error }
    @Environment(\.horusPalette) private var palette
    let tone: Tone
    let title: String
    let detail: String
    var progress = false
    var action: (String, @MainActor () -> Void)?

    var body: some View {
        HStack(spacing: 12) {
            if progress { ProgressView().controlSize(.small) }
            else { HorusIcon(name: symbol, foreground: color) }
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(HorusStyle.controlFont)
                Text(detail).font(HorusStyle.bodyFont).foregroundStyle(palette.muted)
            }
            Spacer()
            if let action {
                Button(action.0, action: action.1)
                    .buttonStyle(.glass)
                    .buttonBorderShape(.capsule)
            }
        }
        .padding(13)
        .background(color.opacity(0.09), in: RoundedRectangle(cornerRadius: HorusStyle.cardRadius))
        .overlay {
            RoundedRectangle(cornerRadius: HorusStyle.cardRadius)
                .stroke(color.opacity(0.45), lineWidth: HorusStyle.borderWidth)
        }
    }

    private var color: Color {
        switch tone {
        case .neutral: palette.accent
        case .success: palette.signal
        case .warning: palette.warning
        case .error: palette.danger
        }
    }

    private var symbol: String {
        switch tone {
        case .neutral: "info"
        case .success: "badge-check"
        case .warning: "triangle-alert"
        case .error: "octagon-x"
        }
    }
}

private func compact(_ value: Int) -> String {
    value.formatted(.number.notation(.compactName).precision(.fractionLength(0 ... 1)))
}

func cacheHit(_ usage: TokenUsage) -> String {
    guard usage.inputTokens > 0 else { return "—" }
    return (Double(usage.cachedInputTokens) / Double(usage.inputTokens))
        .formatted(.percent.precision(.fractionLength(1)))
}

private extension String {
    var nonEmpty: String? {
        let trimmed = trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}
