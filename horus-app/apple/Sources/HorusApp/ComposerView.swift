import Foundation
import SwiftUI
@preconcurrency import AVFoundation

struct ComposerView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: HorusSpace.s) {
            ForEach(model.composerHeaderWidgets) { widget in
                FrontendWidgetView(widget: widget)
            }
            if let approval = model.pendingApproval {
                ApprovalView(approval: approval)
            }
            if let picker = model.pendingPicker {
                FrontendPickerView(picker: picker)
            }
            ComposerStack()
        }
        .frame(maxWidth: 880)
        .frame(maxWidth: .infinity)
        .padding(.horizontal, HorusSpace.l)
        .padding(.bottom, HorusSpace.m)
    }
}

private struct ComposerStack: View {
    var body: some View {
        VStack(spacing: HorusSpace.xs) {
            ComposerActivityView()
            ComposerSurface()
        }
    }
}

private struct ComposerSurface: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase
    @State private var dictation = ComposerDictation()
    @State private var selection: TextSelection?
    @FocusState private var isComposerFocused: Bool
    @State private var referenceSuggestions: ReferenceSuggestions?

    var body: some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            if !model.composerAttachments.isEmpty {
                ComposerAttachmentsView()
                    .padding(.horizontal, HorusSpace.m)
                    .padding(.top, HorusSpace.m)
            }
            TextField(
                "You can just do things",
                text: $model.composer,
                selection: $selection,
                axis: .vertical
            )
            .textFieldStyle(.plain)
            .focused($isComposerFocused)
            .lineLimit(1...8)
            .scrollDismissesKeyboard(.interactively)
            .font(HorusStyle.bodyFont)
            .accessibilityLabel("Message")
            .disabled(dictation.isActive)
            .onSubmit(submit)
            .onKeyPress(.return, phases: .down) { keyPress in
                if keyPress.modifiers.contains(.shift) {
                    insertLineBreak()
                } else {
                    submit()
                }
                return .handled
            }
            .padding(.horizontal, HorusSpace.l)
            .padding(.top, HorusSpace.m)
            .padding(.bottom, HorusSpace.xs)
            ComposerOptionsView(dictation: dictation, selection: $selection)
                .padding(.horizontal, HorusStyle.iconRowPadding)
                .padding(.bottom, HorusStyle.iconRowPadding)
        }
        .horusGlass(in: HorusStyle.cardShape, interactive: true)
        .shadow(color: .black.opacity(0.18), radius: 12, y: 6)
        .overlay(alignment: .top) {
            if let suggestions = referenceSuggestions {
                ReferenceSuggestionsPopup(suggestions: suggestions) {
                    complete($0, suggestions: suggestions)
                }
                .padding(.horizontal, HorusSpace.s)
                .zIndex(2)
            }
        }
        .task(id: referenceSuggestionRequest) {
            let request = referenceSuggestionRequest
            referenceSuggestions = nil
            guard !request.isDisabled else { return }
            try? await Task.sleep(for: .milliseconds(80))
            guard !Task.isCancelled else { return }
            let references = model.capabilityReferences
            let files = model.workspaceFiles
            let searchTask = Task.detached(priority: .userInitiated) {
                AppModel.referenceSuggestions(
                    in: request.text,
                    cursorOffset: request.cursorOffset,
                    capabilityReferences: references,
                    workspaceFiles: files
                )
            }
            let result = await searchTask.value
            guard !Task.isCancelled else { return }
            referenceSuggestions = result
        }
        .onChange(of: model.composerFocusRequest) { _, _ in
            isComposerFocused = true
        }
        .onChange(of: model.composerBlurRequest) { _, _ in
            isComposerFocused = false
        }
        .onChange(of: scenePhase) { _, phase in
            guard phase == .background else { return }
            Task { await dictation.cancel() }
        }
        .onChange(of: model.selectedSessionID) { _, _ in
            Task { await dictation.cancel() }
        }
        .onChange(of: model.connectionState.isReady) { _, isReady in
            guard !isReady else { return }
            Task { await dictation.cancel() }
        }
        .onReceive(
            NotificationCenter.default.publisher(for: AVAudioSession.interruptionNotification)
        ) { notification in
            guard let rawValue = notification.userInfo?[AVAudioSessionInterruptionTypeKey]
                as? UInt,
                  AVAudioSession.InterruptionType(rawValue: rawValue) == .began
            else { return }
            Task { await dictation.cancel() }
        }
        .onDisappear {
            Task { await dictation.cancel() }
        }
    }

    private func submit() {
        guard !dictation.isActive else { return }
        selection = nil
        model.sendMessage()
    }

    private var referenceSuggestionRequest: ReferenceSuggestionRequest {
        let isDisabled = dictation.isActive
        let text = model.composer
        let cursor: String.Index
        if let selection,
           case .selection(let range) = selection.indices,
           range.isEmpty,
           text.indices.contains(range.lowerBound) || range.lowerBound == text.endIndex
        {
            cursor = range.lowerBound
        } else {
            cursor = text.endIndex
        }
        return ReferenceSuggestionRequest(
            text: text,
            cursorOffset: text.distance(from: text.startIndex, to: cursor),
            capabilityRevision: model.contributionsRevision,
            workspaceFileRevision: model.workspaceFilesRevision,
            isDisabled: isDisabled
        )
    }

    private func complete(_ mounted: MountedReference, suggestions: ReferenceSuggestions) {
        guard model.composer == suggestions.source else { return }
        var text = suggestions.source
        let offset = text.distance(from: text.startIndex, to: suggestions.range.lowerBound)
        text.replaceSubrange(suggestions.range, with: mounted.replacement)
        model.composer = text
        selection = TextSelection(insertionPoint: text.index(
            text.startIndex,
            offsetBy: offset + mounted.replacement.count
        ))
    }

    private func insertLineBreak() {
        var text = model.composer
        let range: Range<String.Index>
        if let selection, case .selection(let selectedRange) = selection.indices {
            range = selectedRange
        } else {
            range = text.endIndex..<text.endIndex
        }
        let offset = text.distance(from: text.startIndex, to: range.lowerBound)
        text.replaceSubrange(range, with: "\n")
        model.composer = text
        self.selection = TextSelection(
            insertionPoint: text.index(text.startIndex, offsetBy: offset + 1)
        )
    }
}

private struct ReferenceSuggestionRequest: Equatable, Sendable {
    let text: String
    let cursorOffset: Int
    let capabilityRevision: Int
    let workspaceFileRevision: Int
    let isDisabled: Bool
}

private struct ReferenceSuggestionsPopup: View {
    @Environment(\.horusPalette) private var palette
    let suggestions: ReferenceSuggestions
    let select: (MountedReference) -> Void

    private var height: CGFloat {
        min(CGFloat(suggestions.matches.count) * 48 + 12, 252)
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                ForEach(suggestions.matches) { mounted in
                    Button { select(mounted) } label: {
                        HStack(spacing: HorusSpace.m) {
                            Text(String(mounted.reference.trigger))
                                .font(HorusStyle.controlFont.monospaced().weight(.semibold))
                                .foregroundStyle(palette.accent)
                                .frame(width: 18, alignment: .center)
                            VStack(alignment: .leading, spacing: HorusSpace.xxs) {
                                Text(mounted.reference.value)
                                    .font(HorusStyle.controlFont)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                Text(mounted.reference.description)
                                    .font(HorusStyle.metadataFont)
                                    .foregroundStyle(palette.muted)
                                    .lineLimit(1)
                            }
                            Spacer(minLength: 0)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, HorusSpace.m)
                        .frame(height: 48)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.horusPlain)
                    .help(mounted.reference.description)
                    .accessibilityLabel(mounted.label)
                    .accessibilityHint(mounted.reference.description)
                }
            }
            .padding(.vertical, HorusSpace.s)
        }
        .scrollIndicators(.hidden)
        .frame(height: height)
        .background(palette.panel, in: HorusStyle.tileShape)
        .horusGlass(in: HorusStyle.tileShape)
        .shadow(color: .black.opacity(0.2), radius: 16, y: 8)
        .offset(y: -height - 8)
    }
}

private struct ComposerActivityView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var totals = DiffLineTotals()

    var body: some View {
        GlassEffectContainer(spacing: HorusSpace.s) {
            HStack(spacing: HorusSpace.s) {
                ForEach(model.composerFooterWidgets) { widget in
                    FrontendWidgetView(widget: widget)
                }
                if totals.added > 0 || totals.removed > 0 {
                    Button { model.showFiles(.unstaged) } label: {
                        HStack(spacing: HorusSpace.s) {
                            Text("+\(totals.added)").foregroundStyle(palette.signal)
                            Text("−\(totals.removed)").foregroundStyle(palette.danger)
                        }
                        .font(HorusStyle.badgeFont)
                        .padding(.horizontal, HorusSpace.m)
                        .frame(height: HorusStyle.badgeHeight)
                        .horusGlass(in: Capsule(), interactive: true)
                        .frame(
                            minWidth: HorusStyle.iconButtonSize,
                            minHeight: HorusStyle.iconButtonSize
                        )
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.horusPlain)
                    .accessibilityLabel("Code changes")
                    .accessibilityValue("\(totals.added) additions, \(totals.removed) deletions")
                    .accessibilityHint("Opens modified files")
                }

                SessionStatsBadge()
            }
            .frame(minHeight: HorusStyle.iconButtonSize)
            .scrollableRow()
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .contain)
        .task(id: model.gitDiff) {
            let diff = model.gitDiff
            let countTask = Task.detached(priority: .utility) {
                diffTotals(diff)
            }
            let result = await countTask.value
            guard !Task.isCancelled else { return }
            totals = result
        }
    }

}

/// Context fill and elapsed execution time stay visible; deeper run totals live in the popover.
private struct SessionStatsBadge: View {
    @Environment(AppModel.self) private var model
    @State private var showsDetail = false

    var body: some View {
        if model.selectedSessionID != nil {
            TimelineView(.periodic(from: .now, by: 1)) { timeline in
                let elapsed = model.sessionElapsed(at: timeline.date)
                Button { showsDetail = true } label: {
                    HorusBadge(
                        text: "\(model.contextFillPercent)% · \(formatDuration(elapsed))",
                        progress: model.contextFillFraction,
                        interactive: true
                    )
                    .frame(
                        minWidth: HorusStyle.iconButtonSize,
                        minHeight: HorusStyle.iconButtonSize
                    )
                    .contentShape(Rectangle())
                }
                .buttonStyle(.horusPlain)
                .accessibilityLabel("Session observability")
                .accessibilityValue(
                    "\(model.contextFillPercent) percent context, \(formatDuration(elapsed)) elapsed"
                )
                .sensoryFeedback(.selection, trigger: showsDetail)
                .popover(isPresented: $showsDetail, arrowEdge: .bottom) {
                    BadgePopover(title: "Session") {
                        BadgeStat(
                            label: "Context",
                            value: "\(model.contextTokens.formatted()) · \(model.contextFillPercent)%"
                        )
                        BadgeStat(
                            label: "Compactions",
                            value: model.sessionCompactionCount.formatted()
                        )
                        BadgeStat(label: "Elapsed", value: formatDuration(elapsed))
                        BadgeStat(label: "Runs", value: model.sessionRunCount.formatted())
                        BadgeStat(label: "Model calls", value: model.sessionModelCalls.formatted())
                        BadgeStat(label: "Tool calls", value: model.sessionToolCalls.formatted())
                        BadgeStat(
                            label: "Tool failures",
                            value: model.sessionFailedToolCalls.formatted()
                        )
                        BadgeStat(
                            label: "Run tokens",
                            value: (
                                model.runStats.usage.totalTokens
                                    + (model.runStats.active?.usage.totalTokens ?? 0)
                            ).formatted()
                        )
                        BadgeStat(label: "Cache hit", value: cacheHit(model.lastUsage))
                    }
                }
            }
        }
    }
}

struct BadgePopover<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.m) {
            Text(title)
                .font(HorusStyle.controlFont.weight(.semibold))
            // A full list (every subagent, every file) would otherwise grow the popover
            // past the screen with no way to reach the bottom.
            ScrollView { content }
                .frame(maxHeight: HorusStyle.rowTouch * 8)
                .scrollBounceBehavior(.basedOnSize)
        }
        .padding(HorusSpace.l)
        .frame(minWidth: 220, alignment: .leading)
        .presentationCompactAdaptation(.popover)
    }
}

private struct BadgeStat: View {
    @Environment(\.horusPalette) private var palette
    let label: String
    let value: String

    var body: some View {
        HStack(spacing: HorusSpace.m) {
            Text(label)
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
            Spacer(minLength: HorusSpace.s)
            Text(value)
                .font(HorusStyle.bodyFont.monospacedDigit())
        }
        .accessibilityElement(children: .combine)
    }
}


private struct ComposerAttachmentsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.s) {
            if !model.canSubmitAttachments {
                Text(model.attachmentSubmissionUnavailableMessage)
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            // Tiles are too tall to stack: a few files would push the text field off screen.
            ScrollView(.horizontal) {
                HStack(spacing: HorusSpace.s) {
                    ForEach(model.composerAttachments) { attachment in
                        ComposerAttachmentRow(attachment: attachment)
                    }
                }
            }
            .scrollIndicators(.hidden)
            .scrollBounceBehavior(.basedOnSize)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ComposerAttachmentRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let attachment: ComposerAttachment

    var body: some View {
        FileCard(name: attachment.name, detail: status, detailColor: statusColor) {
            // A tile has no room for a row of controls, so the state sits in the corner and
            // the glyph keeps saying which file this is.
            HStack(spacing: HorusSpace.xxs) {
                stateControl
                Button("Remove attachment", glyph: .x) {
                    model.removeComposerAttachment(attachment.id)
                }
                .labelStyle(.iconOnly)
                .buttonStyle(.horusPlain)
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                .disabled(isUploading)
            }
        }
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private var stateControl: some View {
        switch attachment.state {
        case .queued, .uploading:
            HorusSpinner(size: HorusStyle.glyphInline)
                .frame(width: HorusStyle.rowCompact, height: HorusStyle.rowCompact)
        case .uploaded:
            EmptyView()
        case .failed:
            Button("Retry upload", glyph: .arrowClockwise) {
                model.retryComposerAttachment(attachment.id)
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.horusPlain)
            .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
        }
    }

    private var status: Text {
        switch attachment.state {
        case .queued: Text("Waiting to upload")
        case .uploading: Text("Uploading")
        case .uploaded: Text(attachment.size, format: .byteCount(style: .file))
        case .failed(let message): Text(message)
        }
    }

    private var statusColor: Color {
        if case .failed = attachment.state { return palette.danger }
        return palette.muted
    }

    private var isUploading: Bool {
        if case .uploading = attachment.state { return true }
        return false
    }
}

private struct ApprovalView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let approval: PendingApproval

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.m) {
            HorusLabel(
                title: "Approval required",
                glyph: .shieldCheck,
                iconColor: palette.warning
            )
                .font(HorusStyle.titleFont)
                .foregroundStyle(palette.warning)
            Text(approval.reason).font(HorusStyle.bodyFont)
            ScrollView([.horizontal, .vertical]) {
                LazyVStack(alignment: .leading, spacing: HorusSpace.s) {
                    ForEach(approval.calls) { call in
                        VStack(alignment: .leading, spacing: HorusSpace.xs) {
                            Text(call.name).font(HorusStyle.metadataFont.weight(.bold))
                            Text(call.arguments).font(HorusStyle.metadataFont).textSelection(.enabled)
                        }
                        .padding(HorusSpace.m)
                        .background(palette.raised, in: HorusStyle.controlShape)
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(call.name), arguments \(call.arguments)")
                    }
                }
            }
            .frame(maxHeight: 180)
            ViewThatFits(in: .horizontal) {
                HStack(spacing: HorusSpace.s) { actions }
                VStack(spacing: HorusSpace.s) { actions }.buttonSizing(.flexible)
            }
            .buttonStyle(.horusGlass)
            .buttonBorderShape(.capsule)
            .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .padding(HorusStyle.cardPadding)
        .background(palette.warning.opacity(0.09), in: HorusStyle.cardShape)
        .overlay {
            HorusStyle.cardShape
                .stroke(palette.warning.opacity(0.55), lineWidth: HorusStyle.borderWidth)
        }
    }

    @ViewBuilder
    private var actions: some View {
        Button("Abort", role: .destructive) { model.resolveApproval(.abort) }
        Button("Deny") { model.resolveApproval(.denied(rejection: "Denied in Horus App")) }
        Button("Approve for session") { model.resolveApproval(.approvedForSession) }
        Button("Approve once") { model.resolveApproval(.approved) }
            .horusProminentButton()
    }
}
