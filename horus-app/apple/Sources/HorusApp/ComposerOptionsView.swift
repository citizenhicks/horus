import Foundation
import SwiftUI
import UniformTypeIdentifiers
import CoreTransferable
import PhotosUI

private struct ImportedMediaFile: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .item) { received in
            let directory = URL.temporaryDirectory.appending(
                path: UUID().uuidString,
                directoryHint: .isDirectory
            )
            do {
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                let url = directory.appending(path: received.file.lastPathComponent)
                try FileManager.default.copyItem(at: received.file, to: url)
                return Self(url: url)
            } catch {
                try? FileManager.default.removeItem(at: directory)
                throw error
            }
        }
    }
}

struct ComposerOptionsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let dictation: ComposerDictation
    @Binding var selection: TextSelection?
    @State private var isFileImporterPresented = false
    @State private var isPhotoPickerPresented = false
    @State private var photoSelection: [PhotosPickerItem] = []
    @State private var isFullAccessConfirmationPresented = false

    var body: some View {
        // The icon buttons already pad their own glyphs, so they need no spacing between
        // them: 44pt centres are the native rhythm, and anything more reads as drift.
        HStack(spacing: 0) {
            if model.attachmentsEnabled { addAttachmentControl }
            approvalMenu
            Spacer(minLength: HorusSpace.s)
            modelMenu
            actionButtons
        }
        .fileImporter(
            isPresented: $isFileImporterPresented,
            allowedContentTypes: [.data],
            allowsMultipleSelection: true,
            onCompletion: importFiles
        )
        // The picker runs out of process, so this needs no photo library permission.
        .photosPicker(
            isPresented: $isPhotoPickerPresented,
            selection: $photoSelection,
            maxSelectionCount: 16,
            matching: .any(of: [.images, .videos])
        )
        .onChange(of: photoSelection) { _, items in
            guard !items.isEmpty else { return }
            photoSelection = []
            Task { await importMedia(items) }
        }
        .confirmationDialog(
            "Enable full access?",
            isPresented: $isFullAccessConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Enable Full Access", role: .destructive) {
                model.setApprovalPolicyForCurrentChat("full_access")
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "Shell commands run directly on the gateway host and can read, modify, or "
                    + "delete files, use host services, and access the network without approval. "
                    + "This includes Horus state, TLS credentials, and stored provider "
                    + "credentials. File tools remain limited to the workspace."
            )
        }
    }

    /// The photo library and the file browser are separate pickers, so the plus offers both
    /// rather than assuming every attachment lives in Files.
    @ViewBuilder
    private var addAttachmentControl: some View {
        Menu {
            Button { isPhotoPickerPresented = true } label: {
                HorusLabel(title: "Photos", glyph: .image01)
            }
            Button { isFileImporterPresented = true } label: {
                HorusLabel(title: "Files", glyph: .fileText)
            }
        } label: {
            HorusLabel(
                title: "Add attachment",
                glyph: .plus,
                // A plain menu label gets no disabled treatment, so mute the glyph whenever
                // connection or composer state makes importing unavailable.
                iconColor: model.canImportAttachments ? nil : palette.muted,
                iconSize: HorusStyle.glyphLead
            )
                .labelStyle(.iconOnly)
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .disabled(!model.canImportAttachments)
        .accessibilityLabel("Add attachment")
    }

    private var modelMenu: some View {
        Menu {
            Section("Model") { modelMenuContent }
            Section("Reasoning") { reasoningMenuContent }
        } label: {
            HorusMenuLabel(
                text: currentChoice.map { model.modelLabel(for: $0) } ?? "Model",
                glyph: providerGlyph,
                detail: currentChoice?.reasoningEffort?.capitalized,
                glyphSize: HorusStyle.glyphLead
            )
                .frame(minHeight: HorusStyle.iconButtonSize)
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .sensoryFeedback(.selection, trigger: model.selectedModelRoute)
        .accessibilityLabel("Model and reasoning")
        .accessibilityValue(modelLabel)
    }

    private var approvalMenu: some View {
        Menu {
            Picker("Approval policy", selection: approvalPickerSelection) {
                ForEach(approvalOptions) { option in
                    Text(option.label).tag(option.value)
                }
            }
            .labelsHidden()
        } label: {
            HorusLabel(
                title: approvalLabel,
                glyph: approvalGlyph,
                iconColor: approvalForeground,
                iconSize: HorusStyle.glyphLead
            )
                .labelStyle(.iconOnly)
                .foregroundStyle(approvalForeground)
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .sensoryFeedback(.selection, trigger: approvalValue)
        .disabled(model.agentDraft == nil || approvalOptions.isEmpty)
        .help(approvalLabel)
        .accessibilityLabel("Approval policy")
        .accessibilityValue(approvalLabel)
    }

    @ViewBuilder
    private var modelMenuContent: some View {
        Picker("Model", selection: modelPickerSelection) {
            ForEach(distinctModels, id: \.route) { choice in
                modelMenuOptionLabel(
                    model.modelLabel(for: choice),
                    providerSymbol: model.providerSymbol(for: choice)
                )
                .tag(choice.route)
            }
        }
        .labelsHidden()
    }

    @ViewBuilder
    private var reasoningMenuContent: some View {
        Picker("Reasoning", selection: reasoningPickerSelection) {
            ForEach(reasoningChoices, id: \.route) { choice in
                Text(choice.reasoningEffort?.capitalized ?? "Default")
                    .tag(choice.route)
            }
        }
        .labelsHidden()
    }

    private func modelMenuOptionLabel(
        _ title: String,
        providerSymbol: String?
    ) -> some View {
        Group {
            if let providerSymbol,
               let glyph = HorusSymbol.knownGlyph(for: providerSymbol) {
                HorusLabel(
                    title: title,
                    glyph: glyph
                )
            } else {
                Text(title)
            }
        }
    }

    @ViewBuilder
    private var actionButtons: some View {
        Button(action: toggleDictation) {
            if dictation.isTransitioning {
                ProgressView()
                    .controlSize(.small)
            } else {
                HorusLabel(
                    title: dictationLabel,
                    glyph: .mic01,
                    iconSize: HorusStyle.glyphLead
                )
            }
        }
        .labelStyle(.iconOnly)
        .buttonStyle(HorusIconButtonStyle(prominent: dictation.isRecording, bare: true))
        .disabled(!canToggleDictation)
        .help(dictationLabel)
        .accessibilityLabel(dictationLabel)
        .accessibilityValue(dictationValue)

        if model.activeTurnID != nil && !canSend {
            Button("Stop", glyph: .stopFill) { model.interrupt() }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle(prominent: true))
                .help("Stop")
        } else {
            Button(action: model.sendMessage) {
                Label {
                    Text(model.activeTurnID == nil ? "Send" : "Send steering message")
                } icon: {
                    if isWaitingForGateway {
                        HorusSpinner(
                            size: HorusStyle.iconSize,
                            foreground: palette.onAccent
                        )
                    } else {
                        HorusIcon(
                            model.activeTurnID == nil ? .arrowUp02 : .arrowUpRight01
                        )
                    }
                }
            }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle(prominent: true))
                // `sendMessage()` also needs a session: a gateway with no chats left the button
                // enabled and the tap silent.
                .disabled(!canSend)
                .help(model.activeTurnID == nil ? "Send" : "Send steering message")
        }
    }

    private func importFiles(_ result: Result<[URL], Error>) {
        switch result {
        case .success(let urls):
            Task { await model.importAttachments(urls) }
        case .failure(let error):
            model.showToast(error.localizedDescription, tone: .error)
        }
    }

    /// Keep the filename supplied by Photos while taking the same import path and limits as Files.
    private func importMedia(_ items: [PhotosPickerItem]) async {
        var urls: [URL] = []
        for item in items {
            guard let media = try? await item.loadTransferable(type: ImportedMediaFile.self) else {
                continue
            }
            urls.append(media.url)
        }
        guard !urls.isEmpty else {
            if !items.isEmpty {
                model.showToast("Could not read the selected photos or videos.", tone: .error)
            }
            return
        }
        await model.importAttachments(urls)
        for url in urls { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
    }

    private var currentChoice: ModelChoice? {
        model.modelChoices.first { $0.route == model.selectedModelRoute }
    }

    private var modelPickerSelection: Binding<String> {
        Binding {
            guard let currentChoice else { return "" }
            return distinctModels.first {
                $0.group == currentChoice.group && $0.model == currentChoice.model
            }?.route ?? currentChoice.route
        } set: { route in
            guard let choice = distinctModels.first(where: { $0.route == route }) else { return }
            let effort = currentChoice?.reasoningEffort
            let target = model.modelChoices.first {
                $0.group == choice.group
                    && $0.model == choice.model
                    && $0.reasoningEffort == effort
            } ?? choice
            model.selectModel(target.route)
        }
    }

    private var reasoningPickerSelection: Binding<String> {
        Binding {
            model.selectedModelRoute
        } set: { route in
            model.selectModel(route)
        }
    }

    private var distinctModels: [ModelChoice] {
        var seen = Set<String>()
        return model.modelChoices.filter { seen.insert("\($0.group)\u{0}\($0.model)").inserted }
    }

    private var reasoningChoices: [ModelChoice] {
        guard let currentChoice else { return [] }
        return model.modelChoices.filter {
            $0.group == currentChoice.group && $0.model == currentChoice.model
        }
    }

    private var approvalOptions: [FrontendSettingOption] {
        guard let setting = model.middlewareFeatures
            .first(where: { $0.id == "sandbox" })?
            .settings.first(where: { $0.id == "approval_policy" }),
              case .select(let options, _) = setting.kind
        else { return [] }
        return options
    }

    private var approvalValue: String? {
        guard let value = model.agentDraft?
            .middleware.settings["sandbox"]?["approval_policy"],
              case .string(let policy) = value
        else { return nil }
        return policy
    }

    private var approvalPickerSelection: Binding<String> {
        Binding {
            approvalValue ?? ""
        } set: { policy in
            if policy == "full_access", approvalValue != "full_access" {
                isFullAccessConfirmationPresented = true
            } else {
                model.setApprovalPolicyForCurrentChat(policy)
            }
        }
    }

    private var approvalLabel: String {
        approvalOptions.first(where: { $0.value == approvalValue })?.label ?? "Approval"
    }

    private var approvalGlyph: HorusGlyph {
        switch approvalValue {
        case "ask": .shieldCheck
        case "allow": .shield02
        case "allow_network": .shieldAlert
        case "auto_approve": .aiSecurity02
        case "full_access": .shieldOff
        default: .shieldCheck
        }
    }

    private var approvalForeground: Color {
        guard let approvalValue else { return palette.muted }
        return switch approvalValue {
        case "ask": palette.muted
        case "full_access": palette.danger
        default: palette.warning
        }
    }

    private var modelLabel: String {
        guard let currentChoice else { return "Model" }
        return "\(model.modelLabel(for: currentChoice)) · \(currentChoice.reasoningEffort?.capitalized ?? "Default")"
    }

    private var providerGlyph: HorusGlyph? {
        currentChoice
            .flatMap { model.providerSymbol(for: $0) }
            .flatMap { HorusSymbol.knownGlyph(for: $0) }
    }

    private var canSend: Bool {
        guard model.connectionState.isReady,
              model.canSendComposer,
              !model.composerHasUnfinishedAttachments,
              model.selectedSessionID != nil,
              model.activeTurnID == nil || model.composerAttachments.isEmpty
        else { return false }
        return !dictation.isActive
    }

    private var isWaitingForGateway: Bool {
        switch model.connectionState {
        case .connecting, .authenticating, .loading: true
        case .disconnected, .ready, .failed: false
        }
    }

    private var canToggleDictation: Bool {
        dictation.isRecording
            || dictation.canToggle
                && model.connectionState.isReady
                && model.selectedSessionID != nil
    }

    private var dictationLabel: String {
        switch dictation.state {
        case .idle: "Start dictation"
        case .preparing: "Preparing dictation"
        case .recording: "Stop dictation"
        case .stopping: "Finishing dictation"
        }
    }

    private var dictationValue: String {
        switch dictation.state {
        case .idle: "Not listening"
        case .preparing: "Preparing speech recognition"
        case .recording: "Listening"
        case .stopping: "Finishing transcription"
        }
    }

    private func toggleDictation() {
        Task {
            do {
                if dictation.isRecording {
                    try await dictation.stop()
                } else {
                    let sessionID = model.selectedSessionID
                    try await dictation.start(
                        existingText: model.composer,
                        updateText: { text in
                            guard model.selectedSessionID == sessionID else { return }
                            selection = nil
                            model.composer = text
                        },
                        reportError: { model.showToast($0, tone: .error) }
                    )
                }
            } catch is CancellationError {
                return
            } catch {
                model.showToast(error.localizedDescription, tone: .error)
            }
        }
    }
}
