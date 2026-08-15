import SwiftUI

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
                    HStack(spacing: HorusSpace.xs) {
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
                        HStack(spacing: HorusSpace.xs) {
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
                                HStack(spacing: HorusSpace.xs) {
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
                                HStack(spacing: HorusSpace.xs) {
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
                    HStack(spacing: HorusSpace.xs) {
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
            VStack(alignment: .leading, spacing: HorusSpace.m) {
                Text("Finish \(model.providerLabel(for: provider)) sign-in")
                    .font(HorusStyle.titleFont)
                Text("Open the verification page and enter this code.")
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                Text(code)
                    .font(.system(.title, design: .monospaced, weight: .bold))
                    .tracking(3)
                    .textSelection(.enabled)
                    .padding(HorusSpace.m)
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
