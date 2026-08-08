import Foundation
import SwiftUI
import UniformTypeIdentifiers
#if os(iOS)
import CoreTransferable
import PhotosUI
#endif

#if os(iOS)
private struct ImportedPhotoFile: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .image) { received in
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
#endif

struct ComposerOptionsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    #if os(iOS)
    let dictation: ComposerDictation
    @Binding var selection: TextSelection?
    #endif
    @State private var isFileImporterPresented = false
    #if os(iOS)
    @State private var isPhotoPickerPresented = false
    @State private var photoSelection: [PhotosPickerItem] = []
    #endif

    var body: some View {
        // The icon buttons already pad their own glyphs, so they need no spacing between
        // them: 44pt centres are the native rhythm, and anything more reads as drift.
        HStack(spacing: 0) {
            if model.attachmentsEnabled { addAttachmentControl }
            approvalMenu
            Spacer(minLength: 8)
            modelMenu
            actionButtons
        }
        .fileImporter(
            isPresented: $isFileImporterPresented,
            allowedContentTypes: [.data],
            allowsMultipleSelection: true,
            onCompletion: importFiles
        )
        #if os(iOS)
        // The picker runs out of process, so this needs no photo library permission.
        .photosPicker(
            isPresented: $isPhotoPickerPresented,
            selection: $photoSelection,
            maxSelectionCount: 16,
            matching: .images
        )
        .onChange(of: photoSelection) { _, items in
            guard !items.isEmpty else { return }
            photoSelection = []
            Task { await importPhotos(items) }
        }
        #endif
    }

    /// The photo library and the file browser are separate pickers, so the plus offers both
    /// rather than assuming every attachment lives in Files.
    @ViewBuilder
    private var addAttachmentControl: some View {
        #if os(iOS)
        Menu {
            Button { isPhotoPickerPresented = true } label: {
                HorusPlatformMenuLabel(title: "Photos", glyph: .image01, systemImage: "photo")
            }
            Button { isFileImporterPresented = true } label: {
                HorusPlatformMenuLabel(title: "Files", glyph: .fileText, systemImage: "folder")
            }
        } label: {
            HorusLabel(title: "Add attachment", glyph: .plus)
                .labelStyle(.iconOnly)
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .disabled(!model.canImportAttachments)
        .accessibilityLabel("Add attachment")
        #else
        Button("Add files", glyph: .plus) {
            isFileImporterPresented = true
        }
        .labelStyle(.iconOnly)
        .buttonStyle(HorusIconButtonStyle(bare: true))
        .disabled(!model.canImportAttachments)
        .help("Add files")
        #endif
    }

    private var modelMenu: some View {
        Menu {
            Section("Model") { modelMenuContent }
            Section("Reasoning") { reasoningMenuContent }
        } label: {
            HorusMenuLabel(
                text: currentChoice.map { model.modelLabel(for: $0) } ?? "Model",
                glyph: providerGlyph,
                detail: currentChoice?.reasoningEffort?.capitalized
            )
                .frame(minHeight: HorusStyle.iconButtonSize)
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        #if os(iOS)
        .sensoryFeedback(.selection, trigger: model.selectedModelRoute)
        #endif
        .accessibilityLabel("Model and reasoning")
        .accessibilityValue(modelLabel)
    }

    private var approvalMenu: some View {
        Menu {
            ForEach(approvalOptions) { option in
                Button {
                    model.setApprovalPolicyForCurrentChat(option.value)
                } label: {
                    if option.value == approvalValue {
                        HorusPlatformMenuLabel(
                            title: option.label,
                            glyph: .check,
                            systemImage: "checkmark"
                        )
                    } else {
                        Text(option.label)
                    }
                }
            }
        } label: {
            HorusLabel(
                title: approvalLabel,
                glyph: approvalGlyph,
                iconColor: approvalForeground
            )
                .labelStyle(.iconOnly)
                .foregroundStyle(approvalForeground)
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        #if os(iOS)
        .sensoryFeedback(.selection, trigger: approvalValue)
        #endif
        .disabled(model.agentDraft == nil || approvalOptions.isEmpty)
        .help(approvalLabel)
        .accessibilityLabel("Approval policy")
        .accessibilityValue(approvalLabel)
    }

    @ViewBuilder
    private var modelMenuContent: some View {
        ForEach(distinctModels, id: \.route) { choice in
            Button {
                let effort = currentChoice?.reasoningEffort
                let target = model.modelChoices.first {
                    $0.group == choice.group && $0.model == choice.model && $0.reasoningEffort == effort
                } ?? choice
                model.selectModel(target.route)
            } label: {
                let selected = choice.group == currentChoice?.group
                    && choice.model == currentChoice?.model
                let title = model.modelLabel(for: choice)
                if selected {
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
    }

    @ViewBuilder
    private var reasoningMenuContent: some View {
        ForEach(reasoningChoices, id: \.route) { choice in
            Button {
                model.selectModel(choice.route)
            } label: {
                let selected = choice.route == model.selectedModelRoute
                let title = choice.reasoningEffort?.capitalized ?? "Default"
                if selected {
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
    }

    @ViewBuilder
    private var actionButtons: some View {
        #if os(iOS)
        Button(action: toggleDictation) {
            if dictation.isTransitioning {
                ProgressView()
                    .controlSize(.small)
            } else {
                HorusLabel(title: dictationLabel, glyph: .mic01)
            }
        }
        .labelStyle(.iconOnly)
        .buttonStyle(HorusIconButtonStyle(prominent: dictation.isRecording, bare: true))
        .disabled(!canToggleDictation)
        .help(dictationLabel)
        .accessibilityLabel(dictationLabel)
        .accessibilityValue(dictationValue)
        #endif

        if model.activeTurnID != nil && !canSend {
            Button("Stop", glyph: .stopFill) { model.interrupt() }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle(prominent: true))
                .help("Stop")
        } else {
            Button(
                model.activeTurnID == nil ? "Send" : "Send steering message",
                glyph: model.activeTurnID == nil ? .arrowUp02 : .arrowUpRight01
            ) { model.sendMessage() }
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

    #if os(iOS)
    /// Keep the filename supplied by Photos while taking the same import path and limits as Files.
    private func importPhotos(_ items: [PhotosPickerItem]) async {
        var urls: [URL] = []
        for item in items {
            guard let photo = try? await item.loadTransferable(type: ImportedPhotoFile.self) else {
                continue
            }
            urls.append(photo.url)
        }
        guard !urls.isEmpty else {
            if !items.isEmpty { model.showToast("Could not read the selected photos.", tone: .error) }
            return
        }
        await model.importAttachments(urls)
        for url in urls { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
    }
    #endif

    private var currentChoice: ModelChoice? {
        model.modelChoices.first { $0.route == model.selectedModelRoute }
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

    private var approvalLabel: String {
        approvalOptions.first(where: { $0.value == approvalValue })?.label ?? "Approval"
    }

    private var approvalGlyph: HorusGlyph {
        switch approvalValue {
        case "ask": .shieldCheck
        case "allow": .shield02
        case "allow_network": .shieldAlert
        case "auto_approve": .aiSecurity02
        default: .shieldCheck
        }
    }

    private var approvalForeground: Color {
        guard let approvalValue else { return palette.muted }
        return approvalValue == "ask" ? palette.muted : palette.warning
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
        #if os(iOS)
        return !dictation.isActive
        #else
        return true
        #endif
    }

    #if os(iOS)
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
    #endif
}
