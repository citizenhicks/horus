import SwiftUI

enum AgentSettingsScope: Equatable {
    case gatewayDefault
    case currentChat
}

/// Model and reasoning as two rows, in the composer's glyph-led menu style.
///
/// The gateway advertises one route per model-and-effort pair, so one combined list
/// multiplies every model by every effort and buries the choice that matters. Split, each
/// list stays short and the effort reads as its own decision. The reasoning row appears only
/// when the chosen model actually offers more than one.
struct ModelRoutePicker: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let label: String
    let detail: String
    let choices: [ModelChoice]
    var unsetLabel: String?
    var isEnabled = true
    @Binding var route: String?

    var body: some View {
        LabeledContent {
            Menu {
                Picker(label, selection: modelSelection) {
                    if let unsetLabel {
                        Text(unsetLabel).tag(String?.none)
                    }
                    ForEach(distinctModels, id: \.route) { choice in
                        optionLabel(
                            model.modelLabel(for: choice),
                            symbol: model.providerSymbol(for: choice)
                        )
                        .tag(Optional(choice.route))
                    }
                }
                .labelsHidden()
            } label: {
                menuLabel(selectedModelLabel, glyph: selectedGlyph)
            }
            .menuIndicator(.hidden)
            .buttonStyle(.horusPlain)
            .disabled(!isEnabled)
            .accessibilityLabel(label)
            .accessibilityValue(selectedModelLabel)
        } label: {
            HStack(spacing: 5) {
                Text(label)
                SettingsInfoButton(title: label, detail: detail)
            }
        }
        .sensoryFeedback(.selection, trigger: route)

        if reasoningChoices.count > 1 {
            LabeledContent("Reasoning") {
                Menu {
                    Picker("Reasoning", selection: reasoningSelection) {
                        ForEach(reasoningChoices, id: \.route) { choice in
                            Text(effortLabel(choice)).tag(choice.route)
                        }
                    }
                    .labelsHidden()
                } label: {
                    menuLabel(selected.map(effortLabel) ?? "Default", glyph: nil)
                }
                .menuIndicator(.hidden)
                .buttonStyle(.horusPlain)
                .disabled(!isEnabled)
                .accessibilityLabel("Reasoning")
                .accessibilityValue(selected.map(effortLabel) ?? "Default")
            }
        }
    }

    private func menuLabel(_ text: String, glyph: HorusGlyph?) -> some View {
        HStack(spacing: 5) {
            if let glyph { HorusIcon(glyph, size: 14) }
            Text(text)
                .lineLimit(1)
                .truncationMode(.middle)
            HorusIcon(.caretUpDown, size: 12)
                .accessibilityHidden(true)
        }
        .foregroundStyle(palette.accent)
    }

    @ViewBuilder
    private func optionLabel(_ title: String, symbol: String?) -> some View {
        if let symbol, let glyph = HorusSymbol.knownGlyph(for: symbol) {
            HorusLabel(title: title, glyph: glyph)
        } else {
            Text(title)
        }
    }

    private var selected: ModelChoice? {
        choices.first { $0.route == route }
    }

    private var selectedModelLabel: String {
        guard let selected else { return unsetLabel ?? "Select" }
        return model.modelLabel(for: selected)
    }

    private var selectedGlyph: HorusGlyph? {
        selected
            .flatMap { model.providerSymbol(for: $0) }
            .flatMap { HorusSymbol.knownGlyph(for: $0) }
    }

    private func effortLabel(_ choice: ModelChoice) -> String {
        choice.reasoningEffort?.capitalized ?? "Default"
    }

    private var distinctModels: [ModelChoice] {
        var seen = Set<String>()
        return choices.filter { seen.insert("\($0.group)\u{0}\($0.model)").inserted }
    }

    private var reasoningChoices: [ModelChoice] {
        guard let selected else { return [] }
        return choices.filter { $0.group == selected.group && $0.model == selected.model }
    }

    /// Switching model keeps the effort when the new model offers the same one, so changing
    /// model does not silently reset reasoning to the provider default.
    private var modelSelection: Binding<String?> {
        Binding {
            guard let selected else { return nil }
            return distinctModels.first {
                $0.group == selected.group && $0.model == selected.model
            }?.route ?? selected.route
        } set: { newRoute in
            guard let newRoute, let choice = choices.first(where: { $0.route == newRoute }) else {
                route = nil
                return
            }
            let effort = selected?.reasoningEffort
            route = choices.first {
                $0.group == choice.group
                    && $0.model == choice.model
                    && $0.reasoningEffort == effort
            }?.route ?? choice.route
        }
    }

    private var reasoningSelection: Binding<String> {
        Binding { route ?? "" } set: { route = $0 }
    }
}

struct AgentSettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.horusPalette) private var palette
    @State private var showsAgentStatus = false
    @Namespace private var agentAccessoryNamespace
    let scope: AgentSettingsScope

    var body: some View {
        PageScaffold(
            title: pageTitle,
            detail: pageDetail,
            headerAccessory: { agentStatusAccessory }
        ) {
            if draft != nil {
                Section("System prompt") {
                    // The same glass card the composer uses: this is the one field on the
                    // page you write prose into, and it should feel like the other one.
                    TextField("System prompt", text: systemPrompt, axis: .vertical)
                        .font(HorusStyle.bodyFont)
                        .lineLimit(3...)
                        .textFieldStyle(.plain)
                        .labelsHidden()
                        .accessibilityLabel("System prompt")
                        .padding(.horizontal, 14)
                        .padding(.vertical, 12)
                        .horusGlass(in: HorusStyle.cardShape, interactive: true)
                        .listRowInsets(EdgeInsets(top: 4, leading: 0, bottom: 4, trailing: 0))
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                }

                Section(modelSectionTitle) {
                    ModelRoutePicker(
                        label: "Model",
                        detail: modelSectionDetail,
                        choices: model.modelChoices,
                        isEnabled: !model.modelChoices.isEmpty,
                        route: Binding(
                            get: { selectedModelRoute },
                            set: { if let route = $0 { selectModel(route) } }
                        )
                    )

                    HStack(spacing: 5) {
                        // Hundreds: this ceiling is set in the thousands, and stepping by one
                        // makes the control useless for reaching any value someone wants.
                        Stepper(value: maxModelSteps, in: 1...42_000, step: 100) {
                            Text("Maximum model steps: \(maxModelSteps.wrappedValue.formatted())")
                        }
                        SettingsInfoButton(
                            title: "Maximum model steps",
                            detail: "Maximum primary model rounds allowed in one run before Horus stops it."
                        )
                    }
                    .sensoryFeedback(.selection, trigger: maxModelSteps.wrappedValue)
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
            } else {
                HorusUnavailable(
                    title: unavailableTitle,
                    glyph: .slidersHorizontal,
                    detail: unavailableDetail
                )
            }
        }
    }

    /// The status dot, with the save control splitting out of it once the draft diverges.
    ///
    /// A shared `glassEffectID` namespace is what makes the second glass shape grow out of
    /// the first rather than fade in beside it, so the toolbar reads as one control that
    /// gained an action — and it is the only save affordance on the page, which is why it
    /// stays visible while the form scrolls.
    private var agentStatusAccessory: some View {
        // The container's spacing is a merge threshold, not a gap: at 6 it matched the stack
        // spacing, so the two circles fused into one capsule with an outline drawn round the
        // pair. Zero keeps them separate shapes that still morph from the shared namespace.
        GlassEffectContainer(spacing: 0) {
            // Save sits before the dot: this accessory is pinned to the trailing edge, so
            // growing rightwards would shove the dot inward and the status would appear to
            // move. Leading-side growth leaves the dot where the reader last saw it.
            HStack(spacing: 8) {
                if hasChanges {
                    agentSaveButton
                        .glassEffect(
                            .regular.tint(palette.accentFill).interactive(),
                            in: .circle
                        )
                        .glassEffectID("agent-save", in: agentAccessoryNamespace)
                }
                agentStatusButton
                    .glassEffect(.regular.interactive(), in: .circle)
                    .glassEffectID("agent-status", in: agentAccessoryNamespace)
            }
        }
        .animation(
            reduceMotion ? nil : .spring(response: 0.34, dampingFraction: 0.78),
            value: hasChanges
        )
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
                .contentShape(Circle())
        }
        .buttonStyle(.horusPlain)
        .accessibilityLabel("Agent status")
        .accessibilityValue(agentStatusLabel)
        .help("Agent: \(agentStatusLabel)")
        .popover(isPresented: $showsAgentStatus) {
            agentStatusDetails
        }
    }

    private var agentSaveButton: some View {
        Button(action: applyConfiguration) {
            Group {
                if model.isApplyingConfiguration {
                    HorusSpinner(size: 17, foreground: palette.onAccent)
                } else {
                    HorusIcon(.saveAll, size: 17, foreground: palette.onAccent)
                }
            }
            .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
            .contentShape(Circle())
        }
        .buttonStyle(.horusPlain)
        .disabled(model.isApplyingConfiguration)
        .accessibilityLabel(applyTitle)
        .help(applyTitle)
        .sensoryFeedback(.success, trigger: hasChanges) { was, now in was && !now }
    }

    private var applyTitle: String {
        switch scope {
        case .currentChat: "Apply to this chat"
        case .gatewayDefault: "Save as gateway default"
        }
    }

    private func applyConfiguration() {
        switch scope {
        case .currentChat: model.changeAgentForCurrentChat()
        case .gatewayDefault: model.saveAgentAsDefault()
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
            if case .conflict = applyState {
                Divider()
                Button("Reload") {
                    showsAgentStatus = false
                    reloadDraft()
                }
            }
        }
        .multilineTextAlignment(.center)
        .padding(16)
        .frame(width: 280)
        .presentationCompactAdaptation(.popover)
    }

    private var agentStatusLabel: String {
        guard draft != nil else { return "Unavailable" }
        return switch applyState {
        case .idle, .applied:
            hasChanges ? "Unsaved changes" : "Up to date"
        case .applying: "Applying configuration"
        case .restarting: "Restarting"
        case .busy: "Busy"
        case .conflict: "Changed elsewhere"
        case .invalid: "Configuration rejected"
        case .failed: "Failed"
        }
    }

    private var agentStatusColor: Color {
        guard draft != nil else { return palette.danger }
        return switch applyState {
        case .idle, .applied:
            hasChanges ? palette.warning : palette.signal
        case .applying:
            palette.accent
        case .restarting, .busy, .conflict:
            palette.warning
        case .invalid, .failed:
            palette.danger
        }
    }

    private var agentStatusDetail: String {
        guard draft != nil else { return unavailableDetail }
        return switch applyState {
        case .idle, .applied:
            hasChanges ? unsavedStatusDetail : savedStatusDetail
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
        case .select(let options, let unsetLabel)
            where options.allSatisfy({ option in
                model.modelChoices.contains { $0.route == option.value }
            }) && !options.isEmpty:
            // The gateway advertises reviewer and subagent models as plain selects over
            // routes. They are model choices like any other, so they get the same split.
            ModelRoutePicker(
                label: setting.label,
                detail: setting.description,
                choices: options.compactMap { option in
                    model.modelChoices.first { $0.route == option.value }
                },
                unsetLabel: unsetLabel,
                isEnabled: middlewareEnabled(feature),
                route: selectSetting(feature, setting)
            )
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
            get: { draft?.systemPrompt ?? "" },
            set: { value in updateDraft { $0.systemPrompt = value } }
        )
    }

    private var maxModelSteps: Binding<UInt64> {
        Binding(
            get: { draft?.maxModelSteps ?? 1 },
            set: { value in updateDraft { $0.maxModelSteps = Swift.max(value, 1) } }
        )
    }

    private var defaultModelLabel: String {
        guard let route = selectedModelRoute,
              let choice = model.modelChoices.first(where: { $0.route == route })
        else { return "Select" }
        return "\(model.modelLabel(for: choice)) · \(choice.reasoningEffort?.capitalized ?? "Default")"
    }

    private func modelChoiceLabel(_ choice: ModelChoice) -> String {
        [
            model.providerLabel(for: choice),
            model.modelLabel(for: choice),
            choice.reasoningEffort?.capitalized ?? "Default"
        ].joined(separator: " · ")
    }

    private func middleware(_ feature: MiddlewareFeature) -> Binding<Bool> {
        Binding(
            get: { middlewareEnabled(feature) },
            set: { isEnabled in
                guard !feature.required, var enabled = draft?.middleware.enabled else { return }
                if isEnabled { enabled.insert(feature.id) }
                else { enabled.remove(feature.id) }
                updateDraft { $0.middleware.enabled = enabled }
            }
        )
    }

    private func middlewareEnabled(_ feature: MiddlewareFeature) -> Bool {
        feature.required || (draft?.middleware.enabled.contains(feature.id) ?? false)
    }

    private func integerSetting(
        _ feature: MiddlewareFeature,
        _ setting: FrontendSetting,
        minimum: Int64,
        maximum: Int64?
    ) -> Binding<Int64> {
        Binding(
            get: {
                guard let configured = draft?
                    .middleware.settings[feature.id]?[setting.id],
                    case .integer(let value) = configured
                else { return minimum }
                return value
            },
            set: { value in
                let bounded = maximum.map { Swift.min(Swift.max(value, minimum), $0) }
                    ?? Swift.max(value, minimum)
                updateDraft {
                    $0.middleware.setSetting(
                        .integer(bounded),
                        middleware: feature.id,
                        setting: setting.id
                    )
                }
            }
        )
    }

    private func selectSetting(
        _ feature: MiddlewareFeature,
        _ setting: FrontendSetting
    ) -> Binding<String?> {
        Binding(
            get: {
                guard let configured = draft?
                    .middleware.settings[feature.id]?[setting.id],
                    case .string(let value) = configured
                else { return nil }
                return value
            },
            set: { value in
                updateDraft {
                    $0.middleware.setSetting(
                        value.map(FrontendSettingValue.string),
                        middleware: feature.id,
                        setting: setting.id
                    )
                }
            }
        )
    }

    private var draft: AgentComposition? {
        switch scope {
        case .gatewayDefault: model.defaultAgentDraft
        case .currentChat: model.agentDraft
        }
    }

    private var snapshot: VersionedAgentConfig? {
        switch scope {
        case .gatewayDefault: model.defaultAgentSnapshot
        case .currentChat: model.agentSnapshot
        }
    }

    private var applyState: ApplyState {
        switch scope {
        case .gatewayDefault: model.defaultAgentApplyState
        case .currentChat: model.chatAgentApplyState
        }
    }

    private var selectedModelRoute: String? {
        switch scope {
        case .gatewayDefault: model.defaultAgentDraftModelRoute
        case .currentChat: model.agentDraftModelRoute
        }
    }

    private var hasChanges: Bool {
        guard let snapshot, let draft else { return false }
        return snapshot.config != draft
    }

    private func updateDraft(_ update: (inout AgentComposition) -> Void) {
        guard var draft else { return }
        update(&draft)
        switch scope {
        case .gatewayDefault: model.defaultAgentDraft = draft
        case .currentChat: model.agentDraft = draft
        }
    }

    private func selectModel(_ route: String) {
        switch scope {
        case .gatewayDefault: model.selectDefaultAgentDraftModel(route)
        case .currentChat: model.selectAgentDraftModel(route)
        }
    }

    private func reloadDraft() {
        switch scope {
        case .gatewayDefault: model.reloadDefaultAgentDraft()
        case .currentChat: model.reloadAgentDraft()
        }
    }

    private var pageTitle: String {
        scope == .gatewayDefault ? "Default agent" : "Chat agent"
    }

    private var pageDetail: String {
        switch scope {
        case .gatewayDefault:
            "Set the prompt, model, capabilities, and execution policy inherited by new chats."
        case .currentChat:
            "Change the prompt, model, capabilities, and execution policy for this chat only."
        }
    }

    private var modelSectionTitle: String {
        scope == .gatewayDefault ? "Default AI model" : "Chat AI model"
    }

    private var modelSectionDetail: String {
        switch scope {
        case .gatewayDefault:
            "Sets the provider, model, and reasoning inherited by new chats."
        case .currentChat:
            "Sets the provider, model, and reasoning used by this chat."
        }
    }

    private var unavailableTitle: String {
        scope == .gatewayDefault ? "Default agent unavailable" : "Chat agent unavailable"
    }

    private var unavailableDetail: String {
        guard model.connectionState.isReady else { return "Connect to a gateway first." }
        if scope == .currentChat, model.selectedSessionID == nil { return "Open a chat first." }
        return "Configure a provider first."
    }

    private var unsavedStatusDetail: String {
        scope == .gatewayDefault
            ? "Save this draft as the gateway default for new chats."
            : "Apply this draft to the current chat."
    }

    private var savedStatusDetail: String {
        scope == .gatewayDefault
            ? "The draft matches the gateway default."
            : "The draft matches this chat's saved agent configuration."
    }
}

struct ProvidersView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        PageScaffold(
            title: "Providers",
            detail: ""
        ) {
            if model.providerDraft != nil {
                Section {
                    if configuredProviders.isEmpty {
                        Text("No provider configured on this gateway.")
                            .foregroundStyle(palette.muted)
                    } else {
                        ForEach(configuredProviders) { status in
                            LabeledContent(model.providerLabel(for: status.provider)) {
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
                                Text(model.providerLabel(for: status.provider)).tag(status.provider)
                            }
                        }
                        .labelsHidden()
                        .settingsPickerStyle()
                        .sensoryFeedback(.selection, trigger: providerID.wrappedValue)
                    } label: {
                        HStack(spacing: 5) {
                            Text("Provider")
                            SettingsInfoButton(
                                title: selectedStatus.map { model.providerLabel(for: $0.provider) }
                                    ?? "Provider",
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
                                        detail: "Enter one or more exact provider model IDs separated by commas. Whitespace, empty entries, and duplicates are ignored."
                                    )
                                }
                            }

                            LabeledContent {
                                TextField("low, medium, high", text: providerReasoningEfforts)
                                    .settingsField()
                            } label: {
                                HStack(spacing: 5) {
                                    Text("Reasoning effort(s)")
                                    SettingsInfoButton(
                                        title: "Reasoning effort(s)",
                                        detail: "Enter the exact reasoning efforts supported by these models, separated by commas. Whitespace, empty entries, and duplicates are ignored. Leave empty to use the provider default."
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
            StatusBanner(tone: .success, title: "Credential updated", detail: "\(model.providerLabel(for: provider)) is configured on the gateway.")
        case .startingLogin(let provider):
            StatusBanner(tone: .neutral, title: "Starting \(model.providerLabel(for: provider)) sign-in", detail: "Waiting for a device code.", progress: true)
        case .deviceCode(let provider, let url, let code):
            VStack(alignment: .leading, spacing: 12) {
                Text("Finish \(model.providerLabel(for: provider)) sign-in")
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
            StatusBanner(tone: .success, title: "Sign-in complete", detail: "\(model.providerLabel(for: provider)) is ready on the gateway.")
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

    private var providerReasoningEfforts: Binding<String> {
        Binding(
            get: { model.providerReasoningEffortsText },
            set: { model.updateProviderReasoningEfforts($0) }
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
        if model.defaultAgentSnapshot?.config.provider != draft { return true }
        guard let status = selectedStatus, status.modelIdsConfigurable else { return false }
        return model.providerModelIDs != status.modelIds
            || model.providerReasoningEfforts != status.reasoningEfforts
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
            if !model.isSchedulingEnabled {
                StatusBanner(
                    tone: .neutral,
                    title: "Scheduling is off",
                    detail: "Saved tasks and run history remain visible. Enable Cron in this chat to change or run them."
                )
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
                    .disabled(!model.isSchedulingEnabled)
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
            .disabled(!model.isSchedulingEnabled)
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


/// A compact HugeIcons disclosure for setting guidance that should not permanently occupy a row.
struct SettingsInfoButton: View {
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
                Button("Pair to self-hosted gateway", glyph: .plus) {
                    model.showsPairing = true
                }
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

            Section("Horus Cloud") {
                SettingsCaption("Let Horus provision and manage a private gateway for you, with a 7-day trial and included Luna usage.")
                HorusCloudOfferButton()
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
    let headerAccessory: HeaderAccessory
    let content: Content

    init(
        title: String,
        detail: String,
        @ViewBuilder headerAccessory: () -> HeaderAccessory,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.detail = detail
        self.headerAccessory = headerAccessory()
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                if !detail.isEmpty {
                    Text(detail)
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                }
                content
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .scrollDismissesKeyboard(.interactively)
        }
        .navigationTitle(title)
        .toolbarTitleDisplayMode(.inline)
        .toolbar {
            // iOS 26 wraps a toolbar item in its own glass capsule. These accessories bring
            // their own treatment — the agent pages a pair of glass circles, the rest a bare
            // glyph — and the system's capsule drew a second background around both.
            ToolbarItem(placement: .topBarTrailing) { headerAccessory }
                .sharedBackgroundVisibility(.hidden)
        }
        .background(HorusBackdrop())
    }
}

extension PageScaffold where HeaderAccessory == EmptyView {
    init(
        title: String,
        detail: String,
        @ViewBuilder content: () -> Content
    ) {
        self.init(
            title: title,
            detail: detail,
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

    /// Trailing-aligned entry like Settings.app.
    func settingsField() -> some View {
        multilineTextAlignment(.trailing)
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


func cacheHit(_ usage: TokenUsage) -> String {
    guard usage.inputTokens > 0 else { return "—" }
    return (Double(usage.cachedInputTokens) / Double(usage.inputTokens))
        .formatted(.percent.precision(.fractionLength(1)))
}
