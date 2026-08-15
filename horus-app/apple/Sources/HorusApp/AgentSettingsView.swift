import SwiftUI

enum AgentSettingsScope: Equatable {
    case gatewayDefault
    case currentChat
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
                        .padding(.horizontal, HorusSpace.l)
                        .padding(.vertical, HorusSpace.m)
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

                    HStack(spacing: HorusSpace.xs) {
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
                                .padding(.leading, HorusSpace.m)
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
            HStack(spacing: HorusSpace.s) {
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
                    HorusSpinner(size: HorusStyle.iconSize, foreground: palette.onAccent)
                } else {
                    HorusIcon(.saveAll, size: HorusStyle.iconSize, foreground: palette.onAccent)
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
        VStack(spacing: HorusSpace.m) {
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
        .padding(HorusSpace.l)
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
        HStack(spacing: HorusSpace.xs) {
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
            HStack(spacing: HorusSpace.xs) {
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
                    HStack(spacing: HorusSpace.xs) {
                        Text(selectedLabel)
                        HorusIcon(.caretUpDown, size: HorusStyle.glyphMark, gutter: false)
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
                HStack(spacing: HorusSpace.xs) {
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
