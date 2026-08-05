import SwiftUI
import MarkdownView
#if os(macOS)
import AppKit
#elseif os(iOS)
import UIKit
#endif

struct ChatView: View {
    @Environment(AppModel.self) private var model
    @State private var composerHeight: CGFloat = 0
    @State private var isAtBottom = true
    @State private var scrollToBottomRequest = 0

    var body: some View {
        @Bindable var model = model
        ZStack(alignment: .bottom) {
            TranscriptView(
                bottomInset: composerHeight,
                isAtBottom: $isAtBottom,
                scrollToBottomRequest: scrollToBottomRequest
            )
            ComposerView()
                .onGeometryChange(for: CGFloat.self) { geometry in
                    geometry.size.height
                } action: { height in
                    composerHeight = height
                }
                .zIndex(1)
            if !isAtBottom {
                Button("Scroll to latest", systemImage: "arrow.down") {
                    scrollToBottomRequest += 1
                }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle())
                .padding(.bottom, composerHeight + 12)
                .help("Scroll to latest")
                .zIndex(2)
            }
        }
        .navigationTitle(model.transcript.isEmpty ? "Hello" : model.currentSessionTitle)
        #if os(iOS)
        .toolbarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                inspectorButton
                ChatOptionsMenu()
            }
        }
        .sheet(item: $model.presentedPreview, content: PreviewTranscriptSheet.init)
    }

    private var inspectorButton: some View {
        Button(action: model.toggleInspector) {
            HorusIcon(systemName: "sidebar.right", foreground: .primary)
        }
        .accessibilityLabel("Toggle artifact inspector")
        .tint(.primary)
        .help(model.showsInspector ? "Hide inspector" : "Show inspector")
    }
}

private struct ChatOptionsMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Menu {
            Section(model.workspace?.path ?? "No chat selected") {
                if let git = model.gitStatus, !git.currentBranch.isEmpty {
                    Menu {
                        ForEach(git.branches, id: \.self) { branch in
                            Button {
                                model.switchGitBranch(to: branch)
                            } label: {
                                Label(
                                    branch,
                                    systemImage: branch == git.currentBranch
                                        ? "checkmark"
                                        : "arrow.trianglehead.branch"
                                )
                            }
                            .disabled(branch == git.currentBranch)
                        }
                    } label: {
                        Label(git.currentBranch, systemImage: "arrow.trianglehead.branch")
                    }
                    .disabled(model.isSwitchingGitBranch || !model.canOpenSession)
                }
                Button(action: model.showInspector) {
                    Label("Open code diff", systemImage: "doc.text.magnifyingglass")
                }
                .disabled(model.gitDiff.isEmpty)
                if let path = model.workspace?.path {
                    Button { copyToPasteboard(path) } label: {
                        Label("Copy workspace path", systemImage: "doc.on.doc")
                    }
                }
            }
            Section {
                Button {
                    model.startCronSetup()
                } label: {
                    Label("Schedule as a task…", systemImage: "calendar.badge.clock")
                }
                .disabled(!model.canOpenSession || model.selectedSessionID == nil)
                Button {
                    model.openWorkspaceBrowser()
                } label: {
                    Label("New chat in another folder…", systemImage: "folder.badge.plus")
                }
                .disabled(!model.canCreateSession)
            }
        } label: {
            Image(systemName: "ellipsis")
        }
        .labelStyle(.titleAndIcon)
        .menuIndicator(.hidden)
        .accessibilityLabel("Chat options")
        .tint(.primary)
        .help("Chat options")
    }
}

private struct TranscriptRowLayout {
    let entry: TranscriptEntry
    let topSpacing: CGFloat
}

private struct TranscriptView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let bottomInset: CGFloat
    @Binding var isAtBottom: Bool
    let scrollToBottomRequest: Int
    // A restored transcript lands after the scroll view exists, so an initial-offset anchor
    // resolves against empty content. A bottom-edge scroll position survives the late fill.
    @State private var position = ScrollPosition(edge: .bottom)
    #if os(iOS)
    private let rowSpacing: CGFloat = 12
    private let contentPadding: CGFloat = 16
    #else
    private let rowSpacing: CGFloat = 16
    private let contentPadding: CGFloat = 24
    #endif

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(rows, id: \.entry.id) { row in
                    TranscriptRow(entry: row.entry)
                        .id(row.entry.id)
                        .padding(.top, row.topSpacing)
                }
                Color.clear.frame(height: max(1, bottomInset))
            }
            .frame(maxWidth: 880)
            .frame(maxWidth: .infinity)
            .padding(contentPadding)
        }
        .background(palette.canvas)
        .id(model.selectedSessionID)
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom, for: .sizeChanges)
        .scrollIndicators(.hidden)
        .overlay {
            if model.transcript.isEmpty { emptyState }
        }
        // Measured against the furthest reachable offset, including the bottom inset:
        // comparing the visible rect to the content height never reads as "at bottom".
        .onScrollGeometryChange(for: Bool.self) { geometry in
            // The visible rect covers the toolbar inset that `containerSize` leaves out, so it is
            // the only measure that reaches the content height at rest.
            return geometry.visibleRect.maxY >= geometry.contentSize.height - 24
        } action: { _, atBottom in
            isAtBottom = atBottom
        }
        .onChange(of: scrollToBottomRequest) {
            withAnimation(.easeOut(duration: 0.2)) { position.scrollTo(edge: .bottom) }
        }
        .onChange(of: model.transcript.count) { followTranscript() }
        .onChange(of: model.transcript.last?.text) { followTranscript() }
        .task(id: model.selectedSessionID) { await openAtLatest() }
    }

    /// A lazy stack only estimates the height of rows it has not built, so a single bottom scroll
    /// lands short on a long transcript. Re-assert until the content settles, or the reader scrolls.
    private func openAtLatest() async {
        isAtBottom = true
        for _ in 0..<20 {
            position.scrollTo(edge: .bottom)
            try? await Task.sleep(for: .milliseconds(100))
            if isAtBottom || position.isPositionedByUser { return }
        }
    }

    private func followTranscript() {
        guard isAtBottom || model.activeTurnID != nil else { return }
        position.scrollTo(edge: .bottom)
    }

    // Spacing is resolved up front: a row body must never index back into the live
    // transcript, which can shrink between layout passes.
    private var rows: [TranscriptRowLayout] {
        var previousGroup: String?
        return model.transcript.enumerated().map { index, entry in
            let joinsPrevious = index > 0 && entry.group != nil && entry.group == previousGroup
            previousGroup = entry.group
            return TranscriptRowLayout(
                entry: entry,
                topSpacing: joinsPrevious ? 0 : (index == 0 ? 0 : rowSpacing)
            )
        }
    }

    private var emptyState: some View {
        AgentCard()
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(.horizontal, 24)
            .padding(.bottom, bottomInset)
    }
}

private struct AgentCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        if let config = model.agentSnapshot?.config ?? model.defaultAgentSnapshot?.config {
            HorusCard {
                VStack(spacing: 16) {
                    VStack(spacing: 10) {
                        if let provider = selectedProvider(config) {
                            HorusIcon(
                                systemName: HorusSymbol.systemName(for: provider.symbol),
                                size: 20,
                                foreground: palette.accent
                            )
                                .frame(width: 40, height: 40)
                                .background(palette.accentSoft.opacity(0.65), in: Circle())
                        }
                        VStack(spacing: 3) {
                            Text(selectedModelLabel(config))
                                .font(.headline)
                            Text(modelDetail(config))
                                .font(HorusStyle.bodyFont)
                                .foregroundStyle(palette.muted)
                        }
                        .multilineTextAlignment(.center)
                    }

                    // ponytail: a plain row, not a grid. An adaptive grid pins its columns to the
                    // card's full width, which reads as left-aligned for two or three metrics.
                    HStack(alignment: .top, spacing: 20) {
                        AgentCardMetric(label: "Tools", value: model.toolCount)
                        ForEach(model.middlewareContributionCounts) { contribution in
                            AgentCardMetric(label: contribution.label, value: contribution.value)
                        }
                    }
                    .scrollableRow()

                    VStack(spacing: 10) {
                        AgentCardDetail(label: "Providers", value: configuredProviders)
                        AgentCardDetail(label: "Capabilities", value: activeMiddleware(config))
                        AgentCardDetail(label: "Approval", value: approvalLabel(config.approval))
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .frame(maxWidth: 580)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Current agent configuration")
        }
    }

    private var configuredProviders: String {
        let labels = model.providerStatuses.filter(\.configured).map(\.label)
        return labels.isEmpty ? "None configured" : labels.joined(separator: ", ")
    }

    private func selectedProvider(_ config: AgentComposition) -> ProviderStatus? {
        model.providerStatuses.first { $0.provider == config.provider.provider }
    }

    private func selectedModelLabel(_ config: AgentComposition) -> String {
        let provider = model.providerStatuses.first { $0.provider == config.provider.provider }
        return provider?.models.first { $0.id == config.provider.model }?.label
            ?? config.provider.model
    }

    private func modelDetail(_ config: AgentComposition) -> String {
        let provider = model.providerStatuses.first { $0.provider == config.provider.provider }
        let providerLabel = provider?.label ?? config.provider.provider
        guard let reasoning = config.provider.reasoningEffort else {
            return "\(providerLabel) · Provider-default reasoning"
        }
        let reasoningLabel = provider?.models
            .first { $0.id == config.provider.model }?
            .reasoning.first { $0.id == reasoning }?
            .label ?? reasoning.capitalized
        return "\(providerLabel) · \(reasoningLabel) reasoning"
    }

    private func activeMiddleware(_ config: AgentComposition) -> String {
        let labels = model.middlewareFeatures.compactMap { feature in
            (feature.required || config.middleware.enabled.contains(feature.id))
                ? feature.label
                : nil
        }
        return labels.isEmpty ? "Core only" : labels.joined(separator: ", ")
    }

    private func approvalLabel(_ approval: ApprovalPolicy) -> String {
        switch approval {
        case .on: "Ask before workspace changes"
        case .allow: "Allow changes · no network"
        case .allowNetwork: "Allow changes · network"
        }
    }
}

private struct AgentCardMetric: View {
    @Environment(\.horusPalette) private var palette
    let label: String
    let value: Int

    var body: some View {
        VStack(spacing: 2) {
            Text(value.formatted())
                .font(.title3.weight(.semibold).monospacedDigit())
            Text(label)
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
                .lineLimit(1)
        }
        .frame(minWidth: 88)
        .padding(10)
        .background(palette.raised.opacity(0.55), in: HorusStyle.controlShape)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(label): \(value)")
    }
}

private struct AgentCardDetail: View {
    @Environment(\.horusPalette) private var palette
    let label: String
    let value: String

    var body: some View {
        VStack(spacing: 2) {
            Text(label)
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
            Text(value)
                .font(HorusStyle.bodyFont)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
    }
}

private struct TranscriptRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var isHovered = false
    let entry: TranscriptEntry

    var body: some View {
        Group {
            if hasMessageActions {
                VStack(alignment: actionAlignment, spacing: 0) {
                    content
                    if hasInlineControls {
                        controls
                            .opacity(inlineControlsVisible ? 1 : 0)
                            .allowsHitTesting(inlineControlsVisible)
                            .accessibilityHidden(!inlineControlsVisible)
                    }
                }
                .frame(maxWidth: .infinity, alignment: frameAlignment)
                .animation(.easeOut(duration: 0.12), value: isHovered)
            } else {
                content
            }
        }
        .contentShape(Rectangle())
        .onHover { isHovered = $0 }
        .contextMenu { transcriptActions }
    }

    @ViewBuilder
    private var content: some View {
        switch entry.kind {
        case .user:
            HStack {
                Spacer(minLength: 42)
                Text(entry.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .background(palette.accentSoft, in: HorusStyle.cardShape)
            }
        case .assistant:
            MarkdownText(entry.text, parsesMarkdown: !entry.pending)
                .frame(maxWidth: .infinity, alignment: .leading)
        case .reasoning:
            MarkdownText(entry.text, parsesMarkdown: !entry.pending)
                .foregroundStyle(palette.muted)
            .padding(.leading, 14)
            .overlay(alignment: .leading) {
                Rectangle().fill(palette.line).frame(width: 2)
            }
        case .event, .error:
            if entry.format == "unified_diff" {
                Button { model.selectArtifact(entry.id) } label: {
                    EventCard(entry: entry)
                }
                .buttonStyle(.plain)
                .accessibilityHint("Opens the code diff")
            } else {
                EventCard(entry: entry)
            }
        }
    }

    private var controls: some View {
        HStack(spacing: 0) {
            MessageActionButton(title: "Copy", systemImage: "doc.on.doc") {
                copyToPasteboard(entry.text)
            }
            ForEach(model.messageActionWidgets) { widget in
                MessageActionButton(
                    title: widget.widget.text,
                    systemImage: messageActionSystemImage(widget)
                ) {
                    model.submitWidget(widget)
                }
                .disabled(!model.canOpenSession)
            }
        }
    }

    @ViewBuilder
    private var transcriptActions: some View {
        if hasMessageActions {
            Button("Copy", systemImage: "doc.on.doc") { copyToPasteboard(entry.text) }
            ForEach(model.messageActionWidgets) { widget in
                Button(widget.widget.text, systemImage: messageActionSystemImage(widget)) {
                    model.submitWidget(widget)
                }
                .disabled(!model.canOpenSession)
            }
        }
    }

    private func messageActionSystemImage(_ widget: MountedWidget) -> String {
        widget.widget.symbol.map { HorusSymbol.systemName(for: $0) } ?? "ellipsis"
    }

    private var hasMessageActions: Bool {
        entry.kind == .user || entry.kind == .assistant
    }

    private var hasInlineControls: Bool {
        #if os(macOS)
        true
        #else
        entry.kind == .assistant
        #endif
    }

    private var inlineControlsVisible: Bool {
        entry.kind == .assistant || isHovered
    }

    private var actionAlignment: HorizontalAlignment {
        entry.kind == .user ? .trailing : .leading
    }

    private var frameAlignment: Alignment {
        entry.kind == .user ? .trailing : .leading
    }
}

private struct MessageActionButton: View {
    @Environment(\.horusPalette) private var palette
    @State private var isHovered = false
    let title: String
    let systemImage: String
    let action: () -> Void

    var body: some View {
        Button(title, systemImage: systemImage, action: action)
        .labelStyle(.iconOnly)
        .buttonStyle(.borderless)
        .font(HorusStyle.badgeFont)
        .foregroundStyle(isHovered ? palette.accent : palette.muted)
        .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
        .contentShape(Rectangle())
        .onHover { isHovered = $0 }
        .animation(.easeOut(duration: 0.12), value: isHovered)
        .help(title)
    }
}

private struct EventCard: View {
    @Environment(\.horusPalette) private var palette
    @State private var isExpanded = false
    let entry: TranscriptEntry

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            if entry.pending {
                ProgressView().controlSize(.mini).frame(width: 14, height: 14)
            } else {
                HorusIcon(systemName: systemImage, size: 14, foreground: foreground)
                    .padding(.top, 1)
            }
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(label)
                        .font(HorusStyle.metadataFont.weight(.semibold))
                        .foregroundStyle(foreground)
                    if entry.format == "unified_diff" {
                        Text(diffSummary(entry.text))
                            .font(HorusStyle.metadataFont)
                            .foregroundStyle(palette.muted)
                            .lineLimit(1)
                    }
                }
                if entry.format != "unified_diff" {
                    Text(detail)
                        .font(HorusStyle.metadataFont)
                        .foregroundStyle(entry.tone == "neutral" ? palette.muted : foreground)
                        .lineLimit(isExpanded ? nil : 2)
                        .textSelection(.enabled)
                }
            }
            Spacer(minLength: 0)
            if isTruncatable {
                HorusIcon(
                    systemName: isExpanded ? "chevron.up" : "chevron.down",
                    size: 12,
                    foreground: palette.muted
                )
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .background(
            entry.tone == "neutral" ? palette.panel : foreground.opacity(0.10),
            in: HorusStyle.controlShape
        )
        .contentShape(Rectangle())
        .onTapGesture { if isTruncatable { isExpanded.toggle() } }
    }

    private var detail: String {
        entry.text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var isTruncatable: Bool {
        entry.format != "unified_diff" && detail.contains("\n")
    }

    /// Event ids arrive namespaced as "capability/block", which is the best label available.
    private var label: String {
        if entry.format == "unified_diff" { return "Code change" }
        let capability = entry.id.split(separator: "/").first.map(String.init) ?? ""
        guard !capability.isEmpty, capability.count < 24, !capability.contains("-") else {
            return entry.tone == "error" ? "Error" : "Event"
        }
        return capability.replacingOccurrences(of: "_", with: " ").capitalized
    }

    private var systemImage: String {
        if entry.format == "unified_diff" { return "doc.text.magnifyingglass" }
        switch entry.tone {
        case "success": return "checkmark.circle"
        case "warning": return "exclamationmark.triangle"
        case "error": return "xmark.circle"
        default: return "terminal"
        }
    }

    private var foreground: Color { palette.tone(entry.tone) }
}

private struct MarkdownText: View {
    let text: String
    let parsesMarkdown: Bool

    init(_ text: String, parsesMarkdown: Bool) {
        self.text = text
        self.parsesMarkdown = parsesMarkdown
    }

    var body: some View {
        if parsesMarkdown {
            MarkdownView(text.replacingOccurrences(
                of: #"\\dots\b"#,
                with: #"\\ldots"#,
                options: .regularExpression
            ))
                #if os(iOS)
                .markdownFontGroup(HorusMarkdownFonts())
                #endif
                .markdownMathRenderingEnabled()
                .markdownTableStyle(.github)
                .markdownBlockQuoteStyle(.github)
                .markdownCodeBlockStyle(.default(lightTheme: "xcode", darkTheme: "dark"))
                .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            Text(text).textSelection(.enabled)
        }
    }
}

#if os(iOS)
private struct HorusMarkdownFonts: MarkdownFontGroup {
    var h1: any CustomCTFontConvertible { Font.title3.weight(.semibold) }
    var h2: any CustomCTFontConvertible { Font.headline }
    var h3: any CustomCTFontConvertible { Font.subheadline.weight(.semibold) }
    var body: any CustomCTFontConvertible { HorusStyle.bodyFont }
    var blockQuote: any CustomCTFontConvertible { HorusStyle.bodyFont }
    var codeBlock: any CustomCTFontConvertible { Font.footnote.monospaced() }
    var tableBody: any CustomCTFontConvertible { HorusStyle.bodyFont }
    var inlineMath: any CustomCTFontConvertible { HorusStyle.bodyFont }
    var displayMath: any CustomCTFontConvertible { HorusStyle.bodyFont }
}
#endif

struct ComposerView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 8) {
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
        .padding(.horizontal, 16)
        .padding(.bottom, 12)
    }
}

private struct ComposerStack: View {
    var body: some View {
        VStack(spacing: 4) {
            ComposerActivityView()
            ComposerSurface()
        }
    }
}

private struct ComposerSurface: View {
    @Environment(AppModel.self) private var model
    @State private var selection: TextSelection?

    var body: some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            if let suggestions = referenceSuggestions {
                ScrollView(.horizontal) {
                    HStack(spacing: 6) {
                        ForEach(suggestions.matches) { mounted in
                            Button { complete(mounted, suggestions: suggestions) } label: {
                                HorusBadge(
                                    text: mounted.replacement,
                                    tone: "neutral",
                                    interactive: true
                                )
                            }
                            .buttonStyle(.plain)
                            .help(mounted.reference.description)
                            .accessibilityLabel(mounted.replacement)
                            .accessibilityHint(mounted.reference.description)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.top, 10)
                }
                .scrollIndicators(.hidden)
            }
            TextField(
                "Ask Horus to inspect, explain, or change something…",
                text: $model.composer,
                selection: $selection,
                axis: .vertical
            )
            .textFieldStyle(.plain)
            .lineLimit(1...)
            .font(HorusStyle.bodyFont)
            .accessibilityLabel("Message")
            .onSubmit(submit)
            .onKeyPress(.return, phases: .down) { keyPress in
                if keyPress.modifiers.contains(.shift) {
                    insertLineBreak()
                } else {
                    submit()
                }
                return .handled
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            ComposerOptionsView()
                .padding(.horizontal, 16)
                .padding(.bottom, 12)
        }
        .horusGlass(in: HorusStyle.cardShape)
        .shadow(color: .black.opacity(0.18), radius: 12, y: 6)
    }

    private func submit() {
        selection = nil
        model.sendMessage()
    }

    private var referenceSuggestions: ReferenceSuggestions? {
        let cursor: String.Index
        if let selection, case .selection(let range) = selection.indices, range.isEmpty {
            cursor = range.lowerBound
        } else {
            cursor = model.composer.endIndex
        }
        return model.referenceSuggestions(in: model.composer, cursor: cursor)
    }

    private func complete(_ mounted: MountedReference, suggestions: ReferenceSuggestions) {
        var text = model.composer
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

private struct ComposerActivityView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var showsWorkspace = false
    @State private var showsBranch = false

    var body: some View {
        let totals = diffTotals(model.gitDiff)
        GlassEffectContainer(spacing: 8) {
            HStack(spacing: 8) {
                #if os(macOS)
                if let workspace = model.workspace {
                    Button { showsWorkspace = true } label: {
                        HorusBadge(text: "", systemImage: "folder", interactive: true)
                    }
                        .buttonStyle(.plain)
                        .help(workspace.path)
                        .accessibilityLabel("Workspace")
                        .accessibilityValue(workspace.path)
                        .popover(isPresented: $showsWorkspace) {
                            BadgePopover(title: "Folder") {
                                Text(workspace.path)
                                    .font(HorusStyle.bodyFont.monospaced())
                                    .textSelection(.enabled)
                            }
                        }
                }

                if let git = model.gitStatus, !git.currentBranch.isEmpty {
                    Button { showsBranch = true } label: {
                        HorusBadge(
                            text: "",
                            systemImage: "arrow.trianglehead.branch",
                            interactive: true
                        )
                    }
                        .buttonStyle(.plain)
                        .help(git.currentBranch)
                        .accessibilityLabel("Git branch")
                        .accessibilityValue(git.currentBranch)
                        .popover(isPresented: $showsBranch) {
                            BadgePopover(title: "Git branch") {
                                Text(git.currentBranch)
                                    .font(HorusStyle.bodyFont.monospaced())
                                    .textSelection(.enabled)
                            }
                        }
                }
                #endif

                ForEach(model.composerFooterWidgets) { widget in
                    FrontendWidgetView(widget: widget)
                }
                if model.steeringQueued {
                    HorusBadge(text: "Steering queued", tone: "warning")
                }
                if totals.added > 0 || totals.removed > 0 {
                    Button(action: model.showInspector) {
                        HStack(spacing: 6) {
                            Text("+\(totals.added)").foregroundStyle(palette.signal)
                            Text("−\(totals.removed)").foregroundStyle(palette.danger)
                        }
                        .font(HorusStyle.badgeFont)
                        .padding(.horizontal, 11)
                        .frame(height: HorusStyle.badgeHeight)
                        .horusGlass(in: Capsule(), interactive: true)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Code changes")
                    .accessibilityValue("\(totals.added) additions, \(totals.removed) deletions")
                    .accessibilityHint("Opens the latest code diff")
                }

                SessionStatsBadge()
            }
            .frame(minHeight: HorusStyle.iconButtonSize)
            .scrollableRow()
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .contain)
    }

}

/// Context fill and turn duration in one bubble; cache hit joins them in the popover.
private struct SessionStatsBadge: View {
    @Environment(AppModel.self) private var model
    @State private var showsDetail = false

    var body: some View {
        if model.selectedSessionID != nil {
            TimelineView(.periodic(from: .now, by: 1)) { timeline in
                let elapsed = model.sessionElapsed(at: timeline.date)
                Button { showsDetail = true } label: {
                    HorusBadge(
                        text: elapsed >= 1 ? formatDuration(elapsed) : "\(model.contextFillPercent)%",
                        progress: model.contextFillFraction,
                        interactive: true
                    )
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Context and timing")
                .accessibilityValue("\(model.contextFillPercent) percent context, \(formatDuration(elapsed))")
                .popover(isPresented: $showsDetail) {
                    BadgePopover(title: "Session") {
                        BadgeStat(label: "Context", value: "\(model.contextTokens.formatted()) · \(model.contextFillPercent)%")
                        BadgeStat(label: "Cache hit", value: cacheHit(model.lastUsage))
                        BadgeStat(label: "Elapsed", value: formatDuration(elapsed))
                    }
                }
            }
        }
    }
}

private struct BadgePopover<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(HorusStyle.controlFont.weight(.semibold))
            content
        }
        .padding(16)
        .frame(minWidth: 220, alignment: .leading)
        .presentationCompactAdaptation(.popover)
    }
}

private struct BadgeStat: View {
    @Environment(\.horusPalette) private var palette
    let label: String
    let value: String

    var body: some View {
        HStack(spacing: 12) {
            Text(label)
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
            Spacer(minLength: 8)
            Text(value)
                .font(HorusStyle.bodyFont.monospacedDigit())
        }
        .accessibilityElement(children: .combine)
    }
}

private struct ComposerOptionsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        HStack(spacing: 8) {
            modelMenu
            approvalMenu
            Spacer()
            actionButtons
        }
    }

    private var modelMenu: some View {
        Menu {
            Section("Model") { modelMenuContent }
            Section("Reasoning") { reasoningMenuContent }
        } label: {
            HorusMenuLabel(text: modelLabel)
        }
        .buttonStyle(.plain)
        .frame(minHeight: HorusStyle.iconButtonSize)
        .accessibilityLabel("Model and reasoning")
        .accessibilityValue(modelLabel)
    }

    private var approvalMenu: some View {
        Menu { approvalMenuContent } label: {
            HorusLabel(
                title: approvalLabel,
                systemImage: approvalSystemImage,
                iconColor: approvalForeground
            )
                .labelStyle(.iconOnly)
                .foregroundStyle(approvalForeground)
        }
        .buttonStyle(.plain)
        .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
        .disabled(model.agentDraft == nil)
        .help(approvalLabel)
        .accessibilityLabel("Approval policy")
        .accessibilityValue(approvalLabel)
    }

    @ViewBuilder
    private var modelMenuContent: some View {
        ForEach(distinctModels, id: \.route) { choice in
            Button("\(choice.group) · \(choice.model)") {
                let effort = currentChoice?.reasoningEffort
                let target = model.modelChoices.first {
                    $0.group == choice.group && $0.model == choice.model && $0.reasoningEffort == effort
                } ?? choice
                model.selectModel(target.route)
            }
        }
    }

    @ViewBuilder
    private var reasoningMenuContent: some View {
        ForEach(reasoningChoices, id: \.route) { choice in
            Button(choice.reasoningEffort?.capitalized ?? "Default") {
                model.selectModel(choice.route)
            }
        }
    }

    @ViewBuilder
    private var approvalMenuContent: some View {
        Button("Ask") { selectApproval(.on) }
        Button("Allow · no network") { selectApproval(.allow) }
        Button("Allow · network", role: .destructive) { selectApproval(.allowNetwork) }
    }

    @ViewBuilder
    private var actionButtons: some View {
        if model.activeTurnID != nil && !hasComposerText {
            Button("Stop", systemImage: "stop.fill") { model.interrupt() }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle(prominent: true))
                .help("Stop")
        } else {
            Button("Send", systemImage: "arrow.up") { model.sendMessage() }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle(prominent: true))
                .disabled(!model.connectionState.isReady || !hasComposerText)
                .help(model.activeTurnID == nil ? "Send" : "Send steering message")
        }
    }

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

    private var approvalLabel: String {
        approvalPolicyLabel(model.agentDraft?.approval ?? .on)
    }

    private var approvalSystemImage: String {
        approvalPolicySystemImage(model.agentDraft?.approval ?? .on)
    }

    private var approvalForeground: Color {
        model.agentDraft?.approval == .allowNetwork ? palette.warning : palette.muted
    }

    private var modelLabel: String {
        guard let currentChoice else { return "Model" }
        return "\(currentChoice.model) · \(currentChoice.reasoningEffort?.capitalized ?? "Default")"
    }

    private var hasComposerText: Bool {
        !model.composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func selectApproval(_ policy: ApprovalPolicy) {
        model.setApprovalPolicy(policy)
    }
}

private struct ApprovalView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let approval: PendingApproval

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HorusLabel(
                title: "Approval required",
                systemImage: "hand.raised",
                iconColor: palette.warning
            )
                .font(.headline)
                .foregroundStyle(palette.warning)
            Text(approval.reason).font(HorusStyle.bodyFont)
            ScrollView([.horizontal, .vertical]) {
                LazyVStack(alignment: .leading, spacing: 9) {
                    ForEach(approval.calls) { call in
                        VStack(alignment: .leading, spacing: 5) {
                            Text(call.name).font(HorusStyle.metadataFont.weight(.bold))
                            Text(call.arguments).font(HorusStyle.metadataFont).textSelection(.enabled)
                        }
                        .padding(10)
                        .background(palette.raised, in: HorusStyle.controlShape)
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(call.name), arguments \(call.arguments)")
                    }
                }
            }
            .frame(maxHeight: 180)
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 8) { actions }
                VStack(spacing: 8) { actions }.buttonSizing(.flexible)
            }
            .buttonStyle(.glass)
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

struct FrontendWidgetView: View {
    @Environment(AppModel.self) private var model
    @State private var showsDetail = false
    let widget: MountedWidget

    var body: some View {
        if let content = widget.widget.content {
            Button(action: openDetail) {
                badge
                    .frame(
                        minWidth: HorusStyle.iconButtonSize,
                        minHeight: HorusStyle.iconButtonSize
                    )
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(accessibilityTitle)
            .popover(isPresented: $showsDetail) {
                WidgetContentPopover(content: content, isPresented: $showsDetail)
            }
        } else if widget.widget.action != nil {
            Button(action: submit) {
                badge
                    .frame(
                        minWidth: HorusStyle.iconButtonSize,
                        minHeight: HorusStyle.iconButtonSize
                    )
                    .contentShape(Rectangle())
            }
                .buttonStyle(.plain)
                .accessibilityLabel(accessibilityTitle)
        } else {
            badge
                .frame(minHeight: HorusStyle.iconButtonSize)
                .accessibilityLabel(accessibilityTitle)
        }
    }

    /// Widget text can be as terse as a bare count, so the detail title carries the meaning.
    private var accessibilityTitle: String {
        switch widget.widget.content {
        case .blocks(let title, _), .picker(let title, _): "\(title) \(widget.widget.text)"
        case nil: widget.widget.text
        }
    }

    private var badge: HorusBadge {
        HorusBadge(
            text: widget.widget.iconOnly ? "" : widget.widget.text,
            tone: widget.widget.tone,
            systemImage: widget.widget.symbol.map { HorusSymbol.systemName(for: $0) },
            progress: widget.widget.progress?.fraction,
            interactive: widget.widget.content != nil || widget.widget.action != nil
        )
    }

    private func openDetail() { showsDetail = true }
    private func submit() { model.submitWidget(widget) }
}

private struct WidgetContentPopover: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let content: FrontendWidgetContent
    @Binding var isPresented: Bool

    var body: some View {
        switch content {
        case .blocks(let title, let blocks):
            BadgePopover(title: title) {
                ForEach(blocks.enumerated(), id: \.offset) { _, block in
                    PreviewBlockView(block: block)
                }
            }
        case .picker(let title, let options):
            BadgePopover(title: title) {
                ForEach(options) { option in
                    Button { select(option) } label: {
                        HStack(spacing: 12) {
                            Text(option.label)
                                .font(HorusStyle.controlFont)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Text(option.description)
                                .font(HorusStyle.metadataFont)
                                .foregroundStyle(palette.muted)
                        }
                        .frame(minHeight: HorusStyle.iconButtonSize)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    private func select(_ option: FrontendPickerOption) {
        isPresented = false
        model.submitPickerOption(option)
    }
}

private struct FrontendPickerView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let picker: FrontendPickerPrompt

    var body: some View {
        HorusCard {
            VStack(alignment: .leading, spacing: 11) {
                Text(picker.title)
                    .font(.headline)
                ForEach(picker.options) { option in
                    Button { model.submitPickerOption(option) } label: {
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Text(option.label).fontWeight(.semibold)
                            Text(option.description)
                                .font(HorusStyle.bodyFont)
                                .foregroundStyle(palette.muted)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }
}

private func copyToPasteboard(_ text: String) {
    #if os(macOS)
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(text, forType: .string)
    #elseif os(iOS)
    UIPasteboard.general.string = text
    #endif
}

private func diffTotals(_ text: String) -> (added: Int, removed: Int) {
    text.split(separator: "\n", omittingEmptySubsequences: false).reduce(into: (0, 0)) { result, line in
        if line.hasPrefix("+") && !line.hasPrefix("+++") { result.0 += 1 }
        if line.hasPrefix("-") && !line.hasPrefix("---") { result.1 += 1 }
    }
}

private func formatDuration(_ interval: TimeInterval) -> String {
    let seconds = max(0, Int(interval))
    return Duration.seconds(seconds).formatted(.time(pattern: .minuteSecond(padMinuteToLength: 1)))
}

private func approvalPolicyLabel(_ policy: ApprovalPolicy) -> String {
    switch policy {
    case .on: "Ask"
    case .allow: "Allow · no network"
    case .allowNetwork: "Allow · network"
    }
}

private func approvalPolicySystemImage(_ policy: ApprovalPolicy) -> String {
    switch policy {
    case .on: "hand.raised"
    case .allow: "checkmark.shield"
    case .allowNetwork: "globe"
    }
}

private func diffTitle(_ diff: String) -> String {
    for line in diff.split(separator: "\n", omittingEmptySubsequences: false) {
        if line.hasPrefix("+++ b/") { return String(line.dropFirst(6)) }
        if line.hasPrefix("+++ ") { return String(line.dropFirst(4)) }
    }
    return "Code changes"
}

private func diffSummary(_ text: String) -> String {
    let totals = diffTotals(text)
    return "\(diffTitle(text))  ·  +\(totals.added) −\(totals.removed)"
}
