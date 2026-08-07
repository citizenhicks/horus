import SwiftUI

struct AgentSettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.horusPalette) private var palette
    @State private var showsAgentStatus = false

    var body: some View {
        PageScaffold(
            title: "Agent",
            detail: "Change the prompt, capabilities, and execution policy used by the gateway agent.",
            centersContentOnMac: model.agentDraft == nil,
            headerAccessory: { agentStatusButton }
        ) {
            if model.agentDraft != nil {
                Section("System prompt") {
                    TextField("System prompt", text: systemPrompt, axis: .vertical)
                        .font(HorusStyle.bodyFont)
                        .lineLimit(2...)
                        .textFieldStyle(.plain)
                        .labelsHidden()
                        .accessibilityLabel("System prompt")
                }

                Section("Default AI model") {
                    LabeledContent {
                        Menu {
                            ForEach(model.modelChoices) { choice in
                                Button {
                                    model.selectAgentDraftModel(choice.route)
                                } label: {
                                    let title = "\(choice.group) · \(choice.model) · \(choice.reasoningEffort?.capitalized ?? "Default")"
                                    if choice.route == model.agentDraftModelRoute {
                                        HorusPlatformMenuLabel(
                                            title: title,
                                            glyph: .check,
                                            systemImage: "checkmark"
                                        )
                                    } else {
                                        Text(title)
                                    }
                                }
                            }
                        } label: {
                            HStack(spacing: 5) {
                                Text(defaultModelLabel)
                                HorusIcon(.caretUpDown, size: 12)
                            }
                            .foregroundStyle(palette.accent)
                        }
                        .menuIndicator(.hidden)
                        .buttonStyle(.horusPlain)
                        .disabled(model.modelChoices.isEmpty)
                    } label: {
                        HStack(spacing: 5) {
                            Text("Model")
                            SettingsInfoButton(
                                title: "Default AI model",
                                detail: "Sets the provider, model, and reasoning used for new chats when this draft is saved as the gateway default."
                            )
                        }
                    }
                }

                Section("Capabilities") {
                    ForEach(model.middlewareFeatures, id: \.id) { feature in
                        capabilityToggle(feature)
                        ForEach(feature.settings) { setting in
                            middlewareSetting(feature, setting)
                                .padding(.leading, 12)
                        }
                    }
                }
                .toggleStyle(.switch)

                HorusActionRow { agentConfigurationActions }
                    .settingsStandaloneRow()
            } else {
                HorusUnavailable(
                    title: "Agent unavailable",
                    glyph: .slidersHorizontal,
                    detail: model.connectionState.isReady
                        ? "Configure a provider first."
                        : "Connect to a gateway first."
                )
            }
        }
    }

    private var agentStatusButton: some View {
        Button {
            showsAgentStatus = true
        } label: {
            Circle()
                .fill(agentStatusColor)
                .frame(width: 8, height: 8)
                .symbolEffect(
                    .pulse.byLayer,
                    options: .repeat(.continuous),
                    isActive: !reduceMotion
                )
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .accessibilityLabel("Agent status")
        .accessibilityValue(agentStatusLabel)
        .help("Agent: \(agentStatusLabel)")
        .popover(isPresented: $showsAgentStatus) {
            agentStatusDetails
        }
    }

    private var agentStatusDetails: some View {
        VStack(spacing: 10) {
            Text(agentStatusLabel)
                .font(HorusStyle.controlFont.weight(.semibold))
                .foregroundStyle(agentStatusColor)
            Text(agentStatusDetail)
                .font(HorusStyle.bodyFont)
                .foregroundStyle(palette.muted)
            if case .conflict = model.applyState {
                Divider()
                Button("Reload") {
                    showsAgentStatus = false
                    model.reloadAgentDraft()
                }
            }
        }
        .multilineTextAlignment(.center)
        .padding(16)
        .frame(width: 280)
        .presentationCompactAdaptation(.popover)
    }

    private var agentStatusLabel: String {
        guard model.agentDraft != nil else { return "Unavailable" }
        return switch model.applyState {
        case .idle, .applied:
            hasActiveChanges || hasDefaultChanges ? "Unsaved changes" : "Up to date"
        case .applying: "Applying configuration"
        case .restarting: "Restarting"
        case .busy: "Busy"
        case .conflict: "Changed elsewhere"
        case .invalid: "Configuration rejected"
        case .failed: "Failed"
        }
    }

    private var agentStatusColor: Color {
        guard model.agentDraft != nil else { return palette.danger }
        return switch model.applyState {
        case .idle, .applied:
            hasActiveChanges || hasDefaultChanges ? palette.warning : palette.signal
        case .applying:
            palette.accent
        case .restarting, .busy, .conflict:
            palette.warning
        case .invalid, .failed:
            palette.danger
        }
    }

    private var agentStatusDetail: String {
        guard model.agentDraft != nil else {
            return model.connectionState.isReady
                ? "Configure a provider first."
                : "Connect to a gateway first."
        }
        return switch model.applyState {
        case .idle, .applied:
            hasActiveChanges || hasDefaultChanges
                ? "Apply the draft to this chat or save it as the gateway default."
                : "The draft matches the saved agent configuration."
        case .applying:
            "The gateway is validating this revision."
        case .restarting:
            "The gateway accepted the configuration and is reopening the session."
        case .busy(let message), .conflict(let message), .invalid(let message), .failed(let message):
            message
        }
    }

    private func capabilityToggle(_ feature: MiddlewareFeature) -> some View {
        HStack(spacing: 5) {
            Toggle(feature.label, isOn: middleware(feature))
                .disabled(feature.required)
            SettingsInfoButton(title: feature.label, detail: feature.description)
        }
    }

    @ViewBuilder
    private func middlewareSetting(
        _ feature: MiddlewareFeature,
        _ setting: FrontendSetting
    ) -> some View {
        switch setting.kind {
        case .integer(let minimum, let maximum, let step):
            let value = integerSetting(
                feature,
                setting,
                minimum: minimum,
                maximum: maximum
            )
            let increment = Swift.max(Int(clamping: step), 1)
            HStack(spacing: 5) {
                if let maximum {
                    Stepper(
                        value: value,
                        in: minimum...maximum,
                        step: increment
                    ) {
                        Text("\(setting.label): \(value.wrappedValue.formatted())")
                    }
                    .disabled(!middlewareEnabled(feature))
                } else {
                    Stepper(value: value, step: increment) {
                        Text("\(setting.label): \(value.wrappedValue.formatted())")
                    }
                    .disabled(!middlewareEnabled(feature))
                }
                SettingsInfoButton(title: setting.label, detail: setting.description)
            }
            .sensoryFeedback(.selection, trigger: value.wrappedValue)
        case .select(let options, let unsetLabel):
            let selection = selectSetting(feature, setting)
            let selectedDescription = selection.wrappedValue.flatMap { selected in
                options.first { $0.value == selected }?.description
            }
            let selectedLabel = selection.wrappedValue.flatMap { selected in
                options.first { $0.value == selected }?.label ?? selected
            } ?? unsetLabel ?? "Select"
            LabeledContent {
                Menu {
                    Picker(setting.label, selection: selection) {
                        if let unsetLabel {
                            Text(unsetLabel).tag(String?.none)
                        }
                        ForEach(options) { option in
                            Text(option.label).tag(Optional(option.value))
                        }
                    }
                    .labelsHidden()
                } label: {
                    HStack(spacing: 5) {
                        Text(selectedLabel)
                        HorusIcon(.caretUpDown, size: 12)
                            .accessibilityHidden(true)
                    }
                    .foregroundStyle(palette.accent)
                }
                .menuIndicator(.hidden)
                .buttonStyle(.horusPlain)
                .disabled(!middlewareEnabled(feature))
                .accessibilityLabel(setting.label)
                .accessibilityValue(selectedLabel)
            } label: {
                HStack(spacing: 5) {
                    Text(setting.label)
                    SettingsInfoButton(
                        title: setting.label,
                        detail: selectedDescription ?? setting.description
                    )
                }
            }
            .sensoryFeedback(.selection, trigger: selection.wrappedValue)
        }
    }

    private var systemPrompt: Binding<String> {
        Binding(
            get: { model.agentDraft?.systemPrompt ?? "" },
            set: { model.agentDraft?.systemPrompt = $0 }
        )
    }

    private var defaultModelLabel: String {
        guard let route = model.agentDraftModelRoute,
              let choice = model.modelChoices.first(where: { $0.route == route })
        else { return "Select" }
        return "\(choice.model) · \(choice.reasoningEffort?.capitalized ?? "Default")"
    }

    private func middleware(_ feature: MiddlewareFeature) -> Binding<Bool> {
        Binding(
            get: { middlewareEnabled(feature) },
            set: { isEnabled in
                guard !feature.required, var enabled = model.agentDraft?.middleware.enabled else { return }
                if isEnabled { enabled.insert(feature.id) }
                else { enabled.remove(feature.id) }
                model.agentDraft?.middleware.enabled = enabled
            }
        )
    }

    private func middlewareEnabled(_ feature: MiddlewareFeature) -> Bool {
        feature.required || (model.agentDraft?.middleware.enabled.contains(feature.id) ?? false)
    }

    private func integerSetting(
        _ feature: MiddlewareFeature,
        _ setting: FrontendSetting,
        minimum: Int64,
        maximum: Int64?
    ) -> Binding<Int64> {
        Binding(
            get: {
                guard let configured = model.agentDraft?
                    .middleware.settings[feature.id]?[setting.id],
                    case .integer(let value) = configured
                else { return minimum }
                return value
            },
            set: { value in
                let bounded = maximum.map { Swift.min(Swift.max(value, minimum), $0) }
                    ?? Swift.max(value, minimum)
                model.agentDraft?.middleware.setSetting(
                    .integer(bounded),
                    middleware: feature.id,
                    setting: setting.id
                )
            }
        )
    }

    private func selectSetting(
        _ feature: MiddlewareFeature,
        _ setting: FrontendSetting
    ) -> Binding<String?> {
        Binding(
            get: {
                guard let configured = model.agentDraft?
                    .middleware.settings[feature.id]?[setting.id],
                    case .string(let value) = configured
                else { return nil }
                return value
            },
            set: { value in
                model.agentDraft?.middleware.setSetting(
                    value.map(FrontendSettingValue.string),
                    middleware: feature.id,
                    setting: setting.id
                )
            }
        )
    }

    @ViewBuilder
    private var agentConfigurationActions: some View {
        Button(
            "Change for this chat only",
            glyph: .chatDots,
            action: model.changeAgentForCurrentChat
        )
            .horusProminentButton()
            .disabled(!hasActiveChanges || model.isApplyingConfiguration)

        Button(
            "Save as default",
            glyph: .floppyDisk,
            action: model.saveAgentAsDefault
        )
            .disabled(!hasDefaultChanges || model.isApplyingConfiguration)
    }

    private var hasActiveChanges: Bool {
        guard let snapshot = model.agentSnapshot, let draft = model.agentDraft else { return false }
        return snapshot.config != draft
    }

    private var hasDefaultChanges: Bool {
        guard let snapshot = model.defaultAgentSnapshot, let draft = model.agentDraft else {
            return false
        }
        return snapshot.config != draft
    }
}

struct ProvidersView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        PageScaffold(
            title: "Providers",
            detail: "",
            centersContentOnMac: model.providerDraft == nil
        ) {
            if model.providerDraft != nil {
                Section {
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
                } header: {
                    HStack(spacing: 5) {
                        Text("Configured")
                        SettingsInfoButton(
                            title: "Editing configured providers",
                            detail: "Edit provider details by selecting a configured provider below. When saved to the gateway, all supplied information overrides the existing configuration."
                        )
                    }
                }

                // Model and reasoning are picked per chat in the composer; this page only
                // manages what the gateway itself has configured.
                Section("Provider") {
                    LabeledContent {
                        Picker("Provider", selection: providerID) {
                            ForEach(model.providerStatuses) { status in
                                Text(status.label).tag(status.provider)
                            }
                        }
                        .labelsHidden()
                        .settingsPickerStyle()
                        .sensoryFeedback(.selection, trigger: providerID.wrappedValue)
                    } label: {
                        HStack(spacing: 5) {
                            Text("Provider")
                            SettingsInfoButton(
                                title: selectedStatus?.label ?? "Provider",
                                detail: selectedStatus?.description
                                    ?? "Selects the model service configured on this gateway."
                            )
                        }
                    }

                    if let status = selectedStatus {
                        if status.modelIdsConfigurable {
                            LabeledContent {
                                TextField("model-a, model-b", text: providerModelIDs)
                                    .settingsField()
                            } label: {
                                HStack(spacing: 5) {
                                    Text("Model ID(s)")
                                    SettingsInfoButton(
                                        title: "Model ID(s)",
                                        detail: "Enter one or more exact provider model IDs separated by commas. Whitespace and empty entries are ignored."
                                    )
                                }
                            }
                        }

                        if status.defaultBaseUrl != nil {
                            LabeledContent("Base URL") {
                                TextField("Provider endpoint", text: providerBaseURL)
                                    .textContentType(.URL)
                                    .settingsField()
                            }
                        }

                        Picker("Hosted web search", selection: providerWebSearch) {
                            ForEach(status.webSearch) { search in
                                Text(search.label).tag(search)
                            }
                        }
                        .settingsPickerStyle()
                        .sensoryFeedback(.selection, trigger: providerWebSearch.wrappedValue)
                        .disabled(status.webSearch.count == 1)
                        .accessibilityHint(
                            status.webSearch.count == 1
                                ? "This provider does not offer another web search mode."
                                : "Selects the provider-hosted web search mode."
                        )
                    }
                }

                if selectedStatus != nil {
                    Section("Credential") {
                        credentialControls
                    }
                }

                providerActionStatus

                HorusActionRow {
                    Button(
                        "Save to gateway",
                        glyph: .floppyDisk,
                        action: model.saveProviderAsDefault
                    )
                        .horusProminentButton()
                        .disabled(
                            !hasDefaultChanges
                                || !providerConfigurationValid
                                || model.isApplyingConfiguration
                        )
                }
                .settingsStandaloneRow()
            } else {
                HorusUnavailable(
                    title: "Providers unavailable",
                    glyph: .cpu,
                    detail: "Connect to a gateway first."
                )
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

            if status.auth == .apiKey {
                @Bindable var model = model
                LabeledContent {
                    SecureField("New API key", text: $model.providerAPIKey)
                        .textContentType(.password)
                        .settingsField()
                } label: {
                    HStack(spacing: 5) {
                        Text("API key")
                        SettingsInfoButton(
                            title: "API key",
                            detail: "Sent once to the gateway and never returned to this app."
                        )
                    }
                }
                HorusActionRow {
                    Button("Send key to gateway", glyph: .key) {
                        model.saveProviderCredential(provider: status.provider)
                    }
                    .horusProminentButton()
                    .disabled(model.providerAPIKey.isEmpty)
                }
            } else if status.auth == .deviceCode {
                HorusActionRow {
                    Button(
                        "Start device sign-in",
                        glyph: .signIn
                    ) {
                        model.startProviderLogin(provider: status.provider)
                    }
                    .horusProminentButton()
                }
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
                    .background(palette.raised, in: HorusStyle.controlShape)
                deviceCodeActions(url: url, code: code)
            }
        case .loginFinished(let provider):
            StatusBanner(tone: .success, title: "Sign-in complete", detail: "\(provider) is ready on the gateway.")
        case .failed(let message):
            StatusBanner(tone: .error, title: "Provider action failed", detail: message)
        }
    }

    @ViewBuilder
    private func deviceCodeActions(url: String, code: String) -> some View {
        HorusActionRow {
            if let destination = URL(string: url) {
                Link("Open verification page", destination: destination)
            }
            ShareLink("Copy or share code", item: code)
        }
    }

    private var selectedStatus: ProviderStatus? {
        guard let provider = model.providerDraft?.provider else { return nil }
        return model.providerStatuses.first { $0.provider == provider }
    }

    private var configuredProviders: [ProviderStatus] {
        model.providerStatuses.filter(\.configured)
    }

    private var providerID: Binding<String> {
        Binding(
            get: { model.providerDraft?.provider ?? "" },
            set: { model.selectProvider($0) }
        )
    }

    private var providerBaseURL: Binding<String> {
        Binding(
            get: { model.providerDraft?.baseUrl ?? "" },
            set: { model.providerDraft?.baseUrl = $0.nonEmpty }
        )
    }

    private var providerModelIDs: Binding<String> {
        Binding(
            get: { model.providerModelIDsText },
            set: { model.updateProviderModelIDs($0) }
        )
    }

    private var providerWebSearch: Binding<HostedWebSearch> {
        Binding(
            get: { model.providerDraft?.webSearch ?? .off },
            set: { model.providerDraft?.webSearch = $0 }
        )
    }

    private var selectedProviderModel: ProviderModel? {
        guard let status = selectedStatus,
              let modelID = model.providerDraft?.model
        else { return nil }
        return status.models.first { $0.id == modelID }
    }

    private var hasDefaultChanges: Bool {
        guard let draft = model.providerDraft else { return false }
        return model.defaultAgentSnapshot?.config.provider != draft
    }

    private var providerConfigurationValid: Bool {
        guard let provider = model.providerDraft,
              let status = selectedStatus,
              status.configured,
              status.webSearch.contains(provider.webSearch)
        else { return false }
        if status.modelIdsConfigurable { return !model.providerModelIDs.isEmpty }
        guard !provider.model.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !status.models.isEmpty
        else { return false }
        guard let providerModel = selectedProviderModel else { return false }
        return provider.reasoningEffort == nil
            || providerModel.reasoning.contains { $0.id == provider.reasoningEffort }
    }
}

struct CronView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        // Creating a schedule needs a live chat, so that action lives in the chat menu.
        PageScaffold(
            title: "Schedules",
            detail: "Run durable Horus tasks on the gateway workspace, even when this app is closed."
        ) {
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
                    Button("Refresh", glyph: .arrowClockwise) { model.refreshCron() }
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
            VStack(alignment: .leading, spacing: 5) {
                Text(task.task)
                    .font(HorusStyle.bodyFont.weight(.semibold))
                    .textSelection(.enabled)
                Text("ID \(task.id)")
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
            }
            LabeledContent("Schedule") {
                TextField("* * * * *", text: $schedule)
                    .font(HorusStyle.bodyFont.monospaced())
                    .settingsField()
            }
            HorusActionRow(collapsesToIcons: true) {
                Button("Run now", glyph: .playFill) { model.runCron(task) }
                    .horusProminentButton()
                Button("Reschedule", glyph: .clock) {
                    model.rescheduleCron(task, schedule: schedule)
                }
                    .disabled(schedule == task.schedule)
                Button("Delete", glyph: .trash, role: .destructive) {
                    model.deleteCron(task)
                }
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
                    Text(run.status.rawValue.uppercased())
                        .font(HorusStyle.metadataFont.weight(.bold))
                    Text(Date(timeIntervalSince1970: TimeInterval(run.startedAt)), style: .relative)
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                }
                Text("Task \(run.taskId)").font(HorusStyle.metadataFont)
                if let message = run.message {
                    Text(message).font(HorusStyle.bodyFont).foregroundStyle(palette.muted)
                }
                if let sessionID = run.sessionId {
                    Button("Open session") {
                        model.openSession(sessionID)
                        model.destination = .chat
                    }
                    .buttonStyle(.horusGlass)
                    .buttonBorderShape(.capsule)
                    .padding(.top, 2)
                }
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(run.status.rawValue) run for task \(run.taskId)")
    }

    private var statusColor: Color {
        switch run.status {
        case .succeeded: palette.signal
        case .failed: palette.danger
        case .running: palette.accent
        case .skipped: palette.muted
        }
    }
}

struct ProfileView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let usage = model.profile?.dailyUsage ?? []
        PageScaffold(
            title: "Settings",
            detail: ""
        ) {
            Section("Usage") {
                ProfileUsageSection(days: usage)
            }
            if let stats = model.profile?.runStats {
                Section("Runs") {
                    ProfileRunStatsSection(stats: stats)
                }
            }
            Section("Recent runs") {
                ProfileRecentRuns(groups: model.profile?.recentRunGroups ?? [])
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
            // Four fixed columns: an adaptive grid drops to three and orphans the last metric.
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 8), count: 4), spacing: 16) {
                UsageMetric(label: "TOKENS", value: compact(total.totalTokens))
                UsageMetric(label: "INPUT", value: compact(total.inputTokens))
                UsageMetric(label: "OUTPUT", value: compact(total.outputTokens))
                UsageMetric(label: "CACHE", value: cacheHit(total))
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
    let groups: [SessionRunGroup]

    var body: some View {
        if groups.isEmpty {
            Text("No completed runs yet.")
                .font(HorusStyle.bodyFont)
                .foregroundStyle(palette.muted)
        } else {
            ForEach(groups) { group in
                VStack(alignment: .leading, spacing: 0) {
                    Text(group.title)
                        .font(HorusStyle.controlFont)
                        .lineLimit(1)
                        .padding(.bottom, 2)
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
                }
                .padding(.vertical, 4)
            }
        }
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
        #if os(macOS)
        return "Set up Touch ID in System Settings before enabling app lock."
        #else
        return "Set up Face ID or Touch ID in Settings before enabling app lock."
        #endif
    }
}

/// A compact HugeIcons disclosure for setting guidance that should not permanently occupy a row.
private struct SettingsInfoButton: View {
    @Environment(\.horusPalette) private var palette
    @State private var showsDetail = false
    let title: String
    let detail: String

    var body: some View {
        Button {
            showsDetail = true
        } label: {
            HorusIcon(.info, size: 15, foreground: palette.muted)
                .frame(
                    minWidth: HorusStyle.iconButtonSize,
                    minHeight: HorusStyle.iconButtonSize
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .accessibilityLabel("About \(title)")
        .accessibilityHint("Shows setting guidance")
        .help("About \(title)")
        .sensoryFeedback(.selection, trigger: showsDetail)
        .popover(
            isPresented: $showsDetail,
            attachmentAnchor: .rect(.bounds),
            arrowEdge: .bottom
        ) {
            VStack(alignment: .leading, spacing: 8) {
                Text(title)
                    .font(HorusStyle.controlFont.weight(.semibold))
                Text(detail)
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(16)
            .frame(width: 280, alignment: .leading)
            .presentationCompactAdaptation(.popover)
        }
    }
}

struct GatewayView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var confirmsForget = false
    @State private var renameDraft = ""
    @State private var showsRename = false

    var body: some View {
        PageScaffold(
            title: "Gateway",
            detail: "Manage the selected gateway and pair another device."
        ) {
            Section("Connection") {
                Picker("Gateway", selection: Binding(
                    get: { model.selectedAccountID },
                    set: { model.selectAccount($0) }
                )) {
                    ForEach(model.accounts) { account in
                        Text(account.displayName)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .tag(Optional(account.id))
                    }
                }
                .settingsPickerStyle()
                .sensoryFeedback(.selection, trigger: model.selectedAccountID)
                LabeledContent("Status") {
                    HStack(spacing: 7) {
                        Circle()
                            .fill(model.connectionState.isReady ? palette.signal : palette.danger)
                            .frame(width: 7, height: 7)
                        Text(model.connectionState.label)
                    }
                    .font(HorusStyle.controlFont)
                }
                HStack(spacing: 12) {
                    Text("Endpoint")
                    Spacer(minLength: 8)
                    Text(model.selectedAccount?.endpoint.rawValue ?? "—")
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .trailing)
                        .textSelection(.enabled)
                }
                LabeledContent("Transport", value: transportName)
                LabeledContent("Wire protocol", value: "v\(gatewayProtocolVersion)")
            }

            HorusActionRow(collapsesToIcons: true) {
                Button("Reconnect", glyph: .arrowClockwise, action: model.reconnect)
                Button("Add gateway", glyph: .plus) { model.showsPairing = true }
                Button("Rename", glyph: .pencilSimple) {
                    renameDraft = model.selectedAccount?.displayName ?? ""
                    showsRename = true
                }
                .disabled(model.selectedAccount == nil)
                Button("Forget", glyph: .trash, role: .destructive) {
                    confirmsForget = true
                }
            }
            .settingsStandaloneRow()

            Section("Pair another device") {
                SettingsCaption("Ask this gateway for a short-lived code, then enter it with the same gateway address on the other device.")
                if let pairing = model.pairingCodeInfo {
                    Text(pairing.code)
                        .font(.system(.title2, design: .monospaced, weight: .bold))
                        .tracking(3)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .center)
                    LabeledContent("Expires") {
                        Text(pairing.expiresAt, style: .relative)
                    }
                    .foregroundStyle(palette.muted)
                }
            }

            HorusActionRow {
                if let pairing = model.pairingCodeInfo {
                    ShareLink("Copy or share", item: pairing.code)
                } else {
                    Button(
                        "Create one-time code",
                        glyph: .key,
                        action: model.createPairingCode
                    )
                        .horusProminentButton()
                }
            }
            .settingsStandaloneRow()
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
        .alert("Rename gateway", isPresented: $showsRename) {
            TextField("Gateway name", text: $renameDraft)
            Button("Cancel", role: .cancel) {}
            Button("Rename") { model.renameSelectedGateway(renameDraft) }
                .disabled(renameDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    private var transportName: String {
        guard let endpoint = model.selectedAccount?.endpoint else { return "—" }
        if endpoint.usesWebSocket { return "WebSocket TLS" }
        return endpoint.usesTLS ? "TLS" : "Loopback TCP"
    }
}

struct PageScaffold<HeaderAccessory: View, Content: View>: View {
    @Environment(\.horusPalette) private var palette
    let title: String
    let detail: String
    let centersContentOnMac: Bool
    let headerAccessory: HeaderAccessory
    let content: Content

    init(
        title: String,
        detail: String,
        centersContentOnMac: Bool = false,
        @ViewBuilder headerAccessory: () -> HeaderAccessory,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.detail = detail
        self.centersContentOnMac = centersContentOnMac
        self.headerAccessory = headerAccessory()
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            #if os(macOS)
            HStack(alignment: .top, spacing: 16) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(title)
                        .font(.title2.weight(.semibold))
                    if !detail.isEmpty {
                        Text(detail)
                            .font(HorusStyle.bodyFont)
                            .foregroundStyle(palette.muted)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                headerAccessory
            }
            .padding(.horizontal, 38)
            .padding(.top, 20)
            .padding(.bottom, 8)
            #endif

            Form {
                #if os(iOS)
                if !detail.isEmpty {
                    Text(detail)
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                }
                #endif

                #if os(macOS)
                if !centersContentOnMac { content }
                #else
                content
                #endif
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            #if os(iOS)
            .scrollDismissesKeyboard(.interactively)
            #else
            .overlay {
                if centersContentOnMac { content }
            }
            #endif
        }
        #if os(macOS)
        .frame(maxWidth: 780)
        .frame(maxWidth: .infinity)
        #else
        .navigationTitle(title)
        .toolbarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) { headerAccessory }
        }
        #endif
        .background(HorusBackdrop())
    }
}

extension PageScaffold where HeaderAccessory == EmptyView {
    init(
        title: String,
        detail: String,
        centersContentOnMac: Bool = false,
        @ViewBuilder content: () -> Content
    ) {
        self.init(
            title: title,
            detail: detail,
            centersContentOnMac: centersContentOnMac,
            headerAccessory: EmptyView.init,
            content: content
        )
    }
}

/// Secondary explanation under a form control.
private struct SettingsCaption: View {
    @Environment(\.horusPalette) private var palette
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(HorusStyle.bodyFont)
            .foregroundStyle(palette.muted)
            .listRowSeparator(.hidden)
    }
}

private extension View {
    /// A menu keeps the value on its own row without pushing a destination: the
    /// navigation-link style pushes a blank page from a split view's detail column.
    func settingsPickerStyle() -> some View {
        pickerStyle(.menu)
    }

    /// Trailing-aligned entry like Settings.app on iOS, fixed width on macOS.
    func settingsField() -> some View {
        #if os(iOS)
        multilineTextAlignment(.trailing)
        #else
        frame(maxWidth: 300)
        #endif
    }

    func settingsStandaloneRow() -> some View {
        Section {
            frame(maxWidth: .infinity)
                .listRowInsets(EdgeInsets(top: 6, leading: 0, bottom: 6, trailing: 0))
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
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
            else { HorusIcon(glyph, foreground: color) }
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(HorusStyle.controlFont)
                Text(detail).font(HorusStyle.bodyFont).foregroundStyle(palette.muted)
            }
            Spacer()
            if let action {
                Button(action.0, action: action.1)
                    .buttonStyle(.horusGlass)
                    .buttonBorderShape(.capsule)
            }
        }
        .padding(13)
        .background(color.opacity(0.09), in: HorusStyle.cardShape)
        .overlay {
            HorusStyle.cardShape
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

    private var glyph: HorusGlyph {
        switch tone {
        case .neutral: .info
        case .success: .sealCheck
        case .warning: .warning
        case .error: .warningOctagon
        }
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

func cacheHit(_ usage: TokenUsage) -> String {
    guard usage.inputTokens > 0 else { return "—" }
    return (Double(usage.cachedInputTokens) / Double(usage.inputTokens))
        .formatted(.percent.precision(.fractionLength(1)))
}
