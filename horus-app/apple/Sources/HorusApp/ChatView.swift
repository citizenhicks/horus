import Foundation
import SwiftUI
import MarkdownView
#if os(macOS)
import AppKit
#elseif os(iOS)
@preconcurrency import AVFoundation
import UIKit
#endif

extension MountedWidget {
    var glyph: HorusGlyph {
        widget.symbol.map { HorusSymbol.glyph(for: $0) } ?? .squaresFour
    }

    var systemImage: String {
        widget.symbol.map { HorusSymbol.systemImage(for: $0) } ?? "square.grid.2x2"
    }
}

struct ChatView: View {
    @Environment(AppModel.self) private var model
    @State private var composerHeight: CGFloat = 0
    @State private var isAtBottom = true
    @State private var scrollToBottomRequest = 0
    @State private var presentedWidget: MountedWidget?

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
                Button("Scroll to latest", glyph: .arrowDown) {
                    scrollToBottomRequest += 1
                }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle())
                .padding(.bottom, composerHeight + 12)
                .help("Scroll to latest")
                .zIndex(2)
            }
        }
        .navigationTitle(model.displayedTranscript.isEmpty ? "new chat" : model.currentSessionTitle)
        .navigationSubtitle(chatSubtitle)
        #if os(iOS)
        .toolbarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                inspectorButton
            }
            ToolbarItem(placement: .primaryAction) {
                ChatOptionsMenu(presentedWidget: $presentedWidget)
            }
        }
        .sheet(item: $model.presentedPreview, content: PreviewTranscriptSheet.init)
        .sheet(item: $presentedWidget, content: FrontendWidgetSheet.init)
    }

    private var inspectorButton: some View {
        Button(action: model.toggleFilesInspector) {
            HorusIcon(.sidebarSimple, foreground: .primary)
        }
        .accessibilityLabel("Toggle Files")
        .tint(.primary)
        .help(model.showsInspector ? "Hide Files" : "Show Files")
    }

    private var workspaceName: String {
        guard let path = model.workspace?.path else { return "" }
        return path.split { $0 == "/" || $0 == "\\" }.last.map(String.init) ?? path
    }

    private var chatSubtitle: String {
        [workspaceName, model.gatewayMachineName]
            .filter { !$0.isEmpty }
            .joined(separator: " • ")
    }
}

private struct ChatOptionsMenu: View {
    @Environment(AppModel.self) private var model
    @Binding var presentedWidget: MountedWidget?

    var body: some View {
        Menu {
            Section(model.workspace?.path ?? "No chat selected") {
                if let git = model.gitStatus, !git.currentBranch.isEmpty {
                    Menu {
                        ForEach(git.branches, id: \.self) { branch in
                            Button {
                                model.switchGitBranch(to: branch)
                            } label: {
                                HorusPlatformMenuLabel(
                                    title: branch,
                                    glyph: branch == git.currentBranch ? .check : .gitBranch,
                                    systemImage: branch == git.currentBranch
                                        ? "checkmark"
                                        : "arrow.trianglehead.branch"
                                )
                            }
                            .disabled(branch == git.currentBranch)
                        }
                    } label: {
                        HorusPlatformMenuLabel(
                            title: git.currentBranch,
                            glyph: .gitBranch,
                            systemImage: "arrow.trianglehead.branch"
                        )
                    }
                    .disabled(model.isSwitchingGitBranch || !model.canOpenSession)
                }
                Button { model.showFiles() } label: {
                    HorusPlatformMenuLabel(
                        title: "Files",
                        glyph: .fileMagnifyingGlass,
                        systemImage: "doc.text.magnifyingglass"
                    )
                }
                .disabled(model.selectedSessionID == nil || !model.connectionState.isReady)
                if let path = model.workspace?.path {
                    Button { copyToPasteboard(path) } label: {
                        HorusPlatformMenuLabel(
                            title: "Copy workspace path",
                            glyph: .copy,
                            systemImage: "doc.on.doc"
                        )
                    }
                }
            }
            Section {
                ForEach(model.chatMenuWidgets) { widget in
                    Button {
                        activate(widget)
                    } label: {
                        HorusPlatformMenuLabel(
                            title: widget.widget.text,
                            glyph: widget.glyph,
                            systemImage: widget.systemImage
                        )
                    }
                    .disabled(widget.widget.content == nil && widget.widget.action == nil)
                }
                Button {
                    model.startCronSetup()
                } label: {
                    HorusPlatformMenuLabel(
                        title: "Schedule as a task…",
                        glyph: .calendarDots,
                        systemImage: "calendar.badge.clock"
                    )
                }
                .disabled(!model.canOpenSession || model.selectedSessionID == nil)
                Button {
                    model.openWorkspaceBrowser()
                } label: {
                    HorusPlatformMenuLabel(
                        title: "New chat in another folder…",
                        glyph: .folderPlus,
                        systemImage: "folder.badge.plus"
                    )
                }
                .disabled(!model.canCreateSession)
            }
        } label: {
            #if os(macOS)
            Image(systemName: "ellipsis")
            #else
            HorusIcon(.dotsThree)
            #endif
        }
        .labelStyle(.titleAndIcon)
        .menuIndicator(.hidden)
        .accessibilityLabel("Chat options")
        .tint(.primary)
        .help("Chat options")
    }

    private func activate(_ widget: MountedWidget) {
        if widget.widget.action != nil {
            model.submitWidget(widget)
        }
        if widget.widget.content != nil {
            presentedWidget = widget
        }
    }
}

private struct TranscriptRowLayout {
    let entry: TranscriptEntry
    let topSpacing: CGFloat
}

private struct TranscriptView: View {
    @Environment(AppModel.self) private var model
    let bottomInset: CGFloat
    @Binding var isAtBottom: Bool
    let scrollToBottomRequest: Int
    // A restored transcript lands after the scroll view exists, so an initial-offset anchor
    // resolves against empty content. A bottom-edge scroll position survives the late fill.
    @State private var position = ScrollPosition(edge: .bottom)
    @State private var historyAnchorID: String?
    #if os(iOS)
    private let rowSpacing: CGFloat = 12
    private let contentPadding: CGFloat = 16
    #else
    private let rowSpacing: CGFloat = 16
    private let contentPadding: CGFloat = 24
    #endif

    @ViewBuilder
    var body: some View {
        if model.isLoadingTranscript {
            TranscriptLoadingView(bottomInset: bottomInset)
        } else {
            transcript
        }
    }

    private var transcript: some View {
        ScrollView {
            // ponytail: chat rows have wildly different heights, so exact layout avoids the
            // blank gaps produced by LazyVStack estimates. Paginate before making this lazy again.
            VStack(alignment: .leading, spacing: 0) {
                if model.hasEarlierHistory {
                    HStack {
                        Spacer()
                        Button(action: loadEarlierHistory) {
                            if model.isLoadingEarlierHistory {
                                ProgressView()
                                    .controlSize(.small)
                            } else {
                                Label("Load earlier messages", systemImage: "arrow.up")
                            }
                        }
                        .buttonStyle(.borderless)
                        .disabled(!model.canLoadEarlierHistory)
                        Spacer()
                    }
                    .padding(.bottom, rowSpacing)
                }
                ForEach(rows, id: \.entry.id) { row in
                    TranscriptRow(entry: row.entry)
                        .id(row.entry.id)
                        .padding(.top, row.topSpacing)
                }
                ForEach(model.transcriptTailWidgets) { widget in
                    QueuedMessageView(widget: widget)
                        .padding(.top, rowSpacing)
                }
                Color.clear.frame(height: max(1, bottomInset))
            }
            .scrollTargetLayout()
            .frame(maxWidth: 880)
            .frame(maxWidth: .infinity)
            .padding(contentPadding)
        }
        // The keyboard insets this scroll view, so a plain canvas background stops at the
        // keyboard's top edge and the rounded corners expose black. Every other page paints
        // its backdrop the same way, which is why only the chat showed the cut.
        .background(HorusBackdrop())
        .id(model.selectedSessionID)
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom, for: .sizeChanges)
        .scrollIndicators(.hidden)
        .scrollDismissesKeyboard(.interactively)
        .overlay {
            if model.displayedTranscript.isEmpty {
                emptyState
            }
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
        // Streaming deltas land several times per frame, which is more often than `onChange` may
        // fire. Growing content is what `defaultScrollAnchor(.bottom, for: .sizeChanges)` follows,
        // so only a new row needs an explicit scroll.
        .onChange(of: model.displayedTranscript.count) { followTranscript() }
        .onChange(of: model.displayedTranscript.first?.id) { previous, _ in
            guard let historyAnchorID, historyAnchorID == previous else { return }
            position.scrollTo(id: historyAnchorID, anchor: .top)
            self.historyAnchorID = nil
        }
        .onChange(of: model.selectedSessionID) { historyAnchorID = nil }
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

    private func loadEarlierHistory() {
        historyAnchorID = model.displayedTranscript.first?.id
        model.loadEarlierHistory()
    }

    // Spacing is resolved up front: a row body must never index back into the live
    // transcript, which can shrink between layout passes.
    private var rows: [TranscriptRowLayout] {
        var previousGroup: String?
        return model.displayedTranscript.enumerated().map { index, entry in
            let joinsPrevious = index > 0 && entry.group != nil && entry.group == previousGroup
            previousGroup = entry.group
            return TranscriptRowLayout(
                entry: entry,
                topSpacing: joinsPrevious ? 0 : (index == 0 ? 0 : rowSpacing)
            )
        }
    }

    private var emptyState: some View {
        HorusComposingOrb()
            .frame(width: 144, height: 144)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(.bottom, bottomInset)
            .accessibilityHidden(true)
    }
}

private struct TranscriptLoadingView: View {
    let bottomInset: CGFloat

    var body: some View {
        ZStack {
            HorusBackdrop()
            HorusComposingOrb()
                .frame(width: 112, height: 112)
                .offset(y: -bottomInset / 2)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Loading conversation")
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
                UserMessageContent(entry: entry)
            }
        case .assistant:
            VStack(alignment: .leading, spacing: 6) {
                TranscriptFileCards(files: entry.files)
                if !entry.text.isEmpty {
                    MarkdownText(entry.text, streaming: entry.pending)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        case .reasoning:
            MarkdownText(entry.text, streaming: entry.pending)
                .foregroundStyle(palette.muted)
            .padding(.leading, 14)
            .overlay(alignment: .leading) {
                Rectangle().fill(palette.line).frame(width: 2)
            }
        case .event, .error:
            VStack(alignment: .leading, spacing: 6) {
                TranscriptFileCards(files: entry.files)
                if entry.format == "unified_diff" {
                    Button { model.showFiles(.unstaged) } label: {
                        EventCard(entry: entry)
                    }
                    .buttonStyle(.horusPlain)
                    .accessibilityHint("Opens modified files")
                } else if !entry.text.isEmpty {
                    EventCard(entry: entry)
                }
            }
        }
    }

    private var controls: some View {
        HStack(spacing: 0) {
            MessageActionButton(title: "Copy", glyph: .copy) {
                copyToPasteboard(entry.text)
            }
            if let target = entry.messageTarget {
                ForEach(model.messageActionWidgets) { widget in
                    MessageActionButton(
                        title: widget.widget.text,
                        glyph: messageActionGlyph(widget)
                    ) {
                        model.submitMessageAction(widget, target: target)
                    }
                    .disabled(!model.canOpenSession)
                }
            }
        }
    }

    @ViewBuilder
    private var transcriptActions: some View {
        if hasMessageActions {
            Button("Copy", glyph: .copy) { copyToPasteboard(entry.text) }
            if let target = entry.messageTarget {
                ForEach(model.messageActionWidgets) { widget in
                    Button(widget.widget.text, glyph: messageActionGlyph(widget)) {
                        model.submitMessageAction(widget, target: target)
                    }
                    .disabled(!model.canOpenSession)
                }
            }
        }
    }

    private func messageActionGlyph(_ widget: MountedWidget) -> HorusGlyph {
        widget.widget.symbol.map { HorusSymbol.glyph(for: $0) } ?? .dotsThree
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

/// Files sit above the bubble rather than inside it: nesting a bordered card in a
/// filled bubble reads as a box in a box, and the pill carries the same fill so the pair
/// still reads as one message.
private struct UserMessageContent: View {
    @Environment(\.horusPalette) private var palette
    let entry: TranscriptEntry

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            TranscriptFileCards(files: entry.files)
            if !entry.text.isEmpty {
                Text(entry.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .background(palette.accentSoft, in: HorusStyle.cardShape)
            }
        }
    }
}

private struct TranscriptFileCards: View {
    let files: [SessionFileReference]

    var body: some View {
        ForEach(files) { file in
            SessionFileCard(file: file)
        }
    }
}

struct SessionFileCard: View {
    @Environment(AppModel.self) private var model
    let file: SessionFileReference

    var body: some View {
        ZStack(alignment: .topTrailing) {
            Button {
                model.previewSessionFile(file)
            } label: {
                SessionFileCardLabel(file: file)
            }
            .buttonStyle(.horusPlain)
            .disabled(model.isLoadingFilePresentation)
            .accessibilityLabel("Open file \(file.name)")
            .accessibilityHint("Downloads and opens a preview")

            Menu {
                Button("Preview", glyph: file.name.fileGlyph) {
                    model.previewSessionFile(file)
                }
                Button("Share or Save…", glyph: .arrowUpRight01) {
                    model.saveOrShareSessionFile(file)
                }
            } label: {
                HorusIcon(.dotsThree, size: 14)
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.horusPlain)
            .disabled(model.isLoadingFilePresentation)
            .accessibilityLabel("File actions for \(file.name)")
            .help("File actions")
        }
    }
}

private struct QueuedMessageView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let widget: MountedWidget

    var body: some View {
        HStack {
            Spacer(minLength: 42)
            VStack(alignment: .leading, spacing: 5) {
                Text(header)
                    .font(HorusStyle.metadataFont.weight(.semibold))
                    .foregroundStyle(palette.muted)
                Text(widget.widget.text)
                    .font(HorusStyle.bodyFont)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .background(palette.accentSoft.opacity(0.24), in: HorusStyle.cardShape)
            .overlay {
                HorusStyle.cardShape.stroke(
                    palette.accent.opacity(0.42),
                    style: StrokeStyle(lineWidth: 1.25, lineCap: .round, dash: [1, 4])
                )
            }
            .contentShape(HorusStyle.cardShape)
            .contextMenu {
                if editAction != nil {
                    Button("Edit", glyph: .pencilSimple) {
                        model.editWidgetInputInComposer(widget)
                    }
                }
                Button("Copy", glyph: .copy) { copyToPasteboard(widget.widget.text) }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(header): \(widget.widget.text)")
        .accessibilityValue(editAction == nil ? "Queued" : "Queued, editable until sent")
        .accessibilityActions {
            if editAction != nil {
                Button("Edit queued message") { model.editWidgetInputInComposer(widget) }
            }
            Button("Copy queued message") { copyToPasteboard(widget.widget.text) }
        }
    }

    private var header: String {
        let label = model.middlewareFeatures.first { $0.id == widget.capability }?.label
            ?? widget.capability.replacingOccurrences(of: "_", with: " ").capitalized
        return "\(label) message"
    }

    private var editAction: AgentOperation? {
        guard let action = widget.widget.action, action.capabilityInput != nil else { return nil }
        return action
    }

}

private struct SessionFileCardLabel: View {
    @Environment(\.horusPalette) private var palette
    let file: SessionFileReference

    var body: some View {
        FileCard(
            name: file.name,
            detail: Text("\(Text(fileKind(name: file.name, mediaType: file.mediaType))) · \(Text(file.size, format: .byteCount(style: .file)))"),
            detailColor: palette.muted
        )
    }
}

/// The shared shape for a file in the transcript and in the composer: a glyph tile, the
/// name, and one line under it. No thumbnail — the tile carries the weight instead.
private struct FileCard<Trailing: View>: View {
    @Environment(\.horusPalette) private var palette
    let name: String
    let detail: Text
    let detailColor: Color
    let trailing: Trailing

    init(
        name: String,
        detail: Text,
        detailColor: Color,
        @ViewBuilder trailing: () -> Trailing
    ) {
        self.name = name
        self.detail = detail
        self.detailColor = detailColor
        self.trailing = trailing()
    }

    var body: some View {
        VStack(spacing: 0) {
            HorusIcon(.fileText, size: 26, foreground: palette.accent)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            Text(name)
                .font(HorusStyle.badgeFont)
                .lineLimit(1)
                .truncationMode(.middle)
            detail
                .font(HorusStyle.badgeFont)
                .foregroundStyle(detailColor)
                .lineLimit(1)
        }
        .padding(10)
        .frame(width: 136, height: 112)
        .background(palette.raised, in: HorusStyle.tileShape)
        .overlay(alignment: .topTrailing) { trailing.padding(4) }
        .contentShape(HorusStyle.tileShape)
    }
}

/// The extension reads faster than a media type, but a name without one still needs a word.
private func fileKind(name: String, mediaType: String) -> String {
    let ext = URL(fileURLWithPath: name).pathExtension
    if !ext.isEmpty { return ext.uppercased() }
    return mediaType.split(separator: "/").last.map { $0.uppercased() } ?? "File"
}

extension FileCard where Trailing == EmptyView {
    init(name: String, detail: Text, detailColor: Color) {
        self.init(name: name, detail: detail, detailColor: detailColor) { EmptyView() }
    }
}

private struct MessageActionButton: View {
    @Environment(\.horusPalette) private var palette
    @State private var isHovered = false
    let title: String
    let glyph: HorusGlyph
    let action: () -> Void

    var body: some View {
        // Secondary actions, so a smaller glyph in a smaller box than a standalone icon button:
        // the box is what spaces these apart, and the context menu carries the same actions.
        Button(action: action) {
            HorusIcon(
                glyph,
                size: 13,
                foreground: isHovered ? palette.accent : palette.muted
            )
            .frame(width: 26, height: 26)
            .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .onHover { isHovered = $0 }
        .animation(.easeOut(duration: 0.12), value: isHovered)
        .accessibilityLabel(title)
        .help(title)
    }
}

private struct EventCard: View {
    @Environment(\.horusPalette) private var palette
    @State private var isExpanded = false
    let entry: TranscriptEntry

    @ViewBuilder
    var body: some View {
        if isTruncatable {
            Button { isExpanded.toggle() } label: {
                card
            }
            .buttonStyle(.horusPlain)
            .accessibilityHint(isExpanded ? "Collapses details" : "Expands details")
        } else {
            card
        }
    }

    private var card: some View {
        HStack(alignment: .top, spacing: 10) {
            if entry.pending {
                ProgressView().controlSize(.mini).frame(width: 14, height: 14)
            } else {
                HorusIcon(glyph, size: 14, foreground: foreground)
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
                    isExpanded ? .caretUp : .caretDown,
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

    private var glyph: HorusGlyph {
        if entry.format == "unified_diff" { return .fileMagnifyingGlass }
        switch entry.tone {
        case "success": return .checkCircle
        case "warning": return .warning
        case "error": return .xCircle
        default: return .terminalWindow
        }
    }

    private var foreground: Color { palette.tone(entry.tone) }
}

private struct MarkdownText: View {
    let text: String
    let streaming: Bool

    init(_ text: String, streaming: Bool) {
        self.text = text
        self.streaming = streaming
    }

    var body: some View {
        StreamingMarkdown(text: normalizedText, streaming: streaming)
            #if os(iOS)
            .markdownFontGroup(HorusMarkdownFonts())
            #endif
            .markdownMathRenderingEnabled()
            .markdownTableStyle(.github)
            .markdownBlockQuoteStyle(.github)
            .markdownCodeBlockStyle(.default(lightTheme: "xcode", darkTheme: "dark"))
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var normalizedText: String {
        text.replacingOccurrences(
            of: #"\\dots\b"#,
            with: #"\\ldots"#,
            options: .regularExpression
        )
    }
}

private struct StreamingMarkdown: View {
    @State private var source: StreamingMarkdownSource
    let text: String
    let streaming: Bool

    init(text: String, streaming: Bool) {
        self.text = text
        self.streaming = streaming
        _source = State(initialValue: StreamingMarkdownSource(text))
    }

    var body: some View {
        StreamingMarkdownReader(source) { parseResult in
            MarkdownView(parseResult)
        }
        .onChange(of: update, initial: true) { _, update in
            source.text = update.text
            if !update.streaming { source.finishStreaming() }
        }
    }

    private var update: StreamingMarkdownUpdate {
        StreamingMarkdownUpdate(text: text, streaming: streaming)
    }
}

private struct StreamingMarkdownUpdate: Equatable {
    let text: String
    let streaming: Bool
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
    #if os(iOS)
    @Environment(\.scenePhase) private var scenePhase
    @State private var dictation = ComposerDictation()
    #endif
    @State private var selection: TextSelection?
    @FocusState private var isComposerFocused: Bool
    @State private var referenceSuggestions: ReferenceSuggestions?

    var body: some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            if !model.composerAttachments.isEmpty {
                ComposerAttachmentsView()
                    .padding(.horizontal, 12)
                    .padding(.top, 10)
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
            #if os(iOS)
            .disabled(dictation.isActive)
            #endif
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
            .padding(.top, 12)
            .padding(.bottom, 4)
            #if os(iOS)
            ComposerOptionsView(dictation: dictation, selection: $selection)
                .padding(.horizontal, HorusStyle.iconRowPadding)
                .padding(.bottom, HorusStyle.iconRowPadding)
            #else
            ComposerOptionsView()
                .padding(.horizontal, HorusStyle.iconRowPadding)
                .padding(.bottom, HorusStyle.iconRowPadding)
            #endif
        }
        .horusGlass(in: HorusStyle.cardShape, interactive: true)
        .shadow(color: .black.opacity(0.18), radius: 12, y: 6)
        .overlay(alignment: .top) {
            if let suggestions = referenceSuggestions {
                ReferenceSuggestionsPopup(suggestions: suggestions) {
                    complete($0, suggestions: suggestions)
                }
                .padding(.horizontal, 8)
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
        #if os(iOS)
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
        #endif
    }

    private func submit() {
        #if os(iOS)
        guard !dictation.isActive else { return }
        #endif
        selection = nil
        model.sendMessage()
    }

    private var referenceSuggestionRequest: ReferenceSuggestionRequest {
        #if os(iOS)
        let isDisabled = dictation.isActive
        #else
        let isDisabled = false
        #endif
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
                        HStack(spacing: 10) {
                            Text(String(mounted.reference.trigger))
                                .font(HorusStyle.controlFont.monospaced().weight(.semibold))
                                .foregroundStyle(palette.accent)
                                .frame(width: 18, alignment: .center)
                            VStack(alignment: .leading, spacing: 2) {
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
                        .padding(.horizontal, 12)
                        .frame(height: 48)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.horusPlain)
                    .help(mounted.reference.description)
                    .accessibilityLabel(mounted.label)
                    .accessibilityHint(mounted.reference.description)
                }
            }
            .padding(.vertical, 6)
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
    @State private var showsWorkspace = false
    @State private var showsBranch = false
    @State private var totals = DiffLineTotals()

    var body: some View {
        GlassEffectContainer(spacing: 8) {
            HStack(spacing: 8) {
                #if os(macOS)
                if let workspace = model.workspace {
                    Button { showsWorkspace = true } label: {
                        HorusBadge(text: "", glyph: .folder, interactive: true)
                    }
                        .buttonStyle(.horusPlain)
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
                            glyph: .gitBranch,
                            interactive: true
                        )
                    }
                        .buttonStyle(.horusPlain)
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
                if totals.added > 0 || totals.removed > 0 {
                    Button { model.showFiles(.unstaged) } label: {
                        HStack(spacing: 6) {
                            Text("+\(totals.added)").foregroundStyle(palette.signal)
                            Text("−\(totals.removed)").foregroundStyle(palette.danger)
                        }
                        .font(HorusStyle.badgeFont)
                        .padding(.horizontal, 11)
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


private struct ComposerAttachmentsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !model.canSubmitAttachments {
                Text(model.attachmentSubmissionUnavailableMessage)
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            // Tiles are too tall to stack: a few files would push the text field off screen.
            ScrollView(.horizontal) {
                HStack(spacing: 8) {
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
            HStack(spacing: 2) {
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
            ProgressView().controlSize(.small).frame(width: 26, height: 26)
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
        VStack(alignment: .leading, spacing: 12) {
            HorusLabel(
                title: "Approval required",
                glyph: .shieldCheck,
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
            .buttonStyle(.horusPlain)
            .accessibilityLabel(accessibilityTitle)
            .sensoryFeedback(.selection, trigger: showsDetail)
            .popover(isPresented: $showsDetail, arrowEdge: .bottom) {
                WidgetContentPopover(content: content, select: select)
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
            .buttonStyle(.horusPlain)
            .accessibilityLabel(accessibilityTitle)
        } else {
            badge
                .frame(minHeight: HorusStyle.iconButtonSize)
                .accessibilityLabel(accessibilityTitle)
        }
    }

    /// Widget text can be as terse as a bare count, so the detail title carries the meaning.
    private var accessibilityTitle: String {
        widget.widget.content.map { "\($0.title) \(widget.widget.text)" } ?? widget.widget.text
    }

    private var badge: HorusBadge {
        HorusBadge(
            text: widget.widget.iconOnly ? "" : widget.widget.text,
            tone: widget.widget.tone,
            glyph: widget.widget.symbol.map { HorusSymbol.glyph(for: $0) },
            progress: widget.widget.progress?.fraction,
            interactive: widget.widget.content != nil || widget.widget.action != nil
        )
    }

    private func openDetail() { showsDetail = true }
    private func submit() { model.submitWidget(widget) }

    private func select(_ option: FrontendPickerOption) {
        model.submitPickerOption(option)
        showsDetail = false
    }
}

private struct WidgetContentPopover: View {
    let content: FrontendWidgetContent
    let select: (FrontendPickerOption) -> Void

    var body: some View {
        BadgePopover(title: content.title) {
            FrontendWidgetContentView(content: content, select: select)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct FrontendWidgetContentView: View {
    @Environment(\.horusPalette) private var palette
    let content: FrontendWidgetContent
    let select: (FrontendPickerOption) -> Void

    var body: some View {
        switch content {
        case .blocks(_, let blocks):
            ForEach(blocks) { block in
                PreviewBlockView(block: block.block)
                    .padding(.vertical, 8)
                    .listRowBackground(Color.clear)
                    .listRowSeparator(.hidden)
            }
        case .picker(_, let options):
            ForEach(options) { option in
                Button { select(option) } label: {
                    FrontendPickerOptionLabel(option: option)
                }
                .buttonStyle(.horusPlain)
                .accessibilityLabel(option.label)
                .accessibilityValue(option.detail)
                .accessibilityHint(option.description)
            }
        case .actionList(_, let items):
            if items.isEmpty {
                Text("Nothing here yet.")
                    .foregroundStyle(palette.muted)
                    .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize)
            } else {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(items) { item in
                        FrontendActionListRow(item: item)
                    }
                }
            }
        }
    }
}

private struct FrontendActionListRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var pendingAction: PendingAction?
    @State private var editedText = ""
    let item: FrontendActionListItem

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            if let statusGlyph {
                HorusIcon(statusGlyph, size: 15, foreground: statusColor)
                    .frame(width: 20, height: HorusStyle.iconButtonSize)
            }
            Text(item.text)
                .font(HorusStyle.bodyFont)
                .foregroundStyle(item.state == .completed ? palette.muted : .primary)
                .strikethrough(item.state == .completed, color: palette.muted)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize, alignment: .leading)
            if !item.actions.isEmpty {
                #if os(iOS)
            Menu {
                ForEach(item.actions) { action in
                    Button(role: action.tone == "error" ? .destructive : nil) {
                        activate(action)
                    } label: {
                        HorusLabel(
                            title: action.label,
                            glyph: HorusSymbol.glyph(for: action.symbol)
                        )
                    }
                }
            } label: {
                HorusIcon(.dotsThree, foreground: palette.accent)
                    .frame(
                        width: HorusStyle.iconButtonSize,
                        height: HorusStyle.iconButtonSize
                    )
                    .contentShape(Rectangle())
            }
            .accessibilityLabel("More actions")
            .accessibilityHint("Shows available actions for this item")
            .help("More actions")
            #else
            HStack(spacing: 0) {
                ForEach(item.actions) { action in
                    Button(role: action.tone == "error" ? .destructive : nil) {
                        activate(action)
                    } label: {
                        HorusIcon(
                            HorusSymbol.glyph(for: action.symbol),
                            foreground: actionColor(action)
                        )
                        .frame(
                            width: HorusStyle.iconButtonSize,
                            height: HorusStyle.iconButtonSize
                        )
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.horusPlain)
                    .accessibilityLabel(action.label)
                    .help(action.label)
                }
            }
            .fixedSize()
                #endif
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(statusLabel): \(item.text)")
        .alert(
            pendingAction?.action.label ?? "",
            isPresented: isPresentingAction,
            presenting: pendingAction
        ) { pending in
            switch pending.kind {
            case .edit:
                TextField("Text", text: $editedText)
                Button("Cancel", role: .cancel) { pendingAction = nil }
                Button("Save") {
                    model.submitFrontendOperation(
                        pending.action.op.replacingCapabilityInput(with: editedText)
                    )
                    pendingAction = nil
                }
                .disabled(
                    editedText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || editedText == pending.action.op.capabilityInput
                )
            case .destructive:
                Button("Cancel", role: .cancel) { pendingAction = nil }
                Button(pending.action.label, role: .destructive) {
                    model.submitFrontendOperation(pending.action.op)
                    pendingAction = nil
                }
            }
        } message: { pending in
            if pending.kind == .destructive {
                Text(pending.itemText)
            }
        }
    }

    private var isPresentingAction: Binding<Bool> {
        Binding(
            get: { pendingAction != nil },
            set: { if !$0 { pendingAction = nil } }
        )
    }

    private func activate(_ action: FrontendActionListAction) {
        if action.tone == "error" {
            pendingAction = PendingAction(kind: .destructive, itemText: item.text, action: action)
        } else if let input = action.op.capabilityInput {
            editedText = input
            pendingAction = PendingAction(kind: .edit, itemText: item.text, action: action)
        } else {
            model.submitFrontendOperation(action.op)
        }
    }

    private func actionColor(_ action: FrontendActionListAction) -> Color {
        action.tone == "neutral" ? palette.accent : palette.tone(action.tone)
    }

    private var statusGlyph: HorusGlyph? {
        switch item.state {
        case .plain: nil
        case .pending: .clock
        case .inProgress: .arrowClockwise
        case .completed: .checkCircle
        }
    }

    private var statusColor: Color {
        switch item.state {
        case .plain, .pending: palette.muted
        case .inProgress: palette.accent
        case .completed: palette.signal
        }
    }

    private var statusLabel: String {
        switch item.state {
        case .plain: "Item"
        case .pending: "Pending"
        case .inProgress: "In progress"
        case .completed: "Completed"
        }
    }
}

private struct PendingAction {
    enum Kind: Equatable {
        case edit
        case destructive
    }

    let kind: Kind
    let itemText: String
    let action: FrontendActionListAction
}

private struct FrontendPickerOptionLabel: View {
    @Environment(\.horusPalette) private var palette
    let option: FrontendPickerOption

    var body: some View {
        HStack(spacing: 8) {
            Text(option.label)
                .font(HorusStyle.controlFont.weight(.semibold))
                .foregroundStyle(palette.accent)
                .lineLimit(1)
            if !option.description.isEmpty {
                Text(option.description)
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            if !option.detail.isEmpty {
                Text(option.detail)
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
                    .lineLimit(1)
            }
            HorusIcon(.caretRight, size: 12, foreground: palette.muted)
        }
        .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize, alignment: .leading)
        .contentShape(Rectangle())
    }
}

struct FrontendWidgetSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let widget: MountedWidget

    var body: some View {
        NavigationStack {
            List {
                if let content = currentWidget?.widget.content {
                    Section {
                        FrontendWidgetContentView(content: content) { option in
                            model.submitPickerOption(option)
                            dismiss()
                        }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .navigationTitle(currentWidget?.title ?? widget.title)
            .toolbarTitleDisplayMode(.inline)
            #if os(macOS)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done", action: dismiss.callAsFunction)
                }
            }
            #endif
            .background(HorusBackdrop())
        }
        #if os(macOS)
        .frame(minWidth: 520, minHeight: 460)
        #endif
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private var currentWidget: MountedWidget? {
        model.chatMenuWidgets.first { $0.id == widget.id }
    }
}

private struct FrontendPickerView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let picker: FrontendPickerPrompt

    var body: some View {
        HorusCard {
            VStack(alignment: .leading, spacing: 11) {
                HStack {
                    Text(picker.title)
                        .font(.headline)
                    Spacer(minLength: 8)
                    Button { model.pendingPicker = nil } label: {
                        HorusIcon(.x, size: 14, foreground: palette.muted)
                            .frame(
                                width: HorusStyle.iconButtonSize,
                                height: HorusStyle.iconButtonSize
                            )
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.horusPlain)
                    .accessibilityLabel("Dismiss \(picker.title)")
                    .help("Dismiss")
                }
                ForEach(picker.options) { option in
                    Button { model.submitPickerOption(option) } label: {
                        FrontendPickerOptionLabel(option: option)
                    }
                    .buttonStyle(.horusPlain)
                    .accessibilityLabel(option.label)
                    .accessibilityValue(option.detail)
                    .accessibilityHint(option.description)
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

private struct DiffLineTotals: Equatable, Sendable {
    var added = 0
    var removed = 0
}

private func diffTotals(_ text: String) -> DiffLineTotals {
    text.split(separator: "\n", omittingEmptySubsequences: false)
        .reduce(into: DiffLineTotals()) { result, line in
            if line.hasPrefix("+") && !line.hasPrefix("+++") { result.added += 1 }
            if line.hasPrefix("-") && !line.hasPrefix("---") { result.removed += 1 }
        }
}

private func formatDuration(_ interval: TimeInterval) -> String {
    let seconds = max(0, Int(interval))
    return Duration.seconds(seconds).formatted(.time(pattern: .minuteSecond(padMinuteToLength: 1)))
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
