import Foundation
import SwiftUI
import MarkdownView
@preconcurrency import AVFoundation
import UIKit

extension MountedWidget {
    var glyph: HorusGlyph {
        widget.symbol.map { HorusSymbol.glyph(for: $0) } ?? .squaresFour
    }

}

struct ChatView: View {
    @Environment(AppModel.self) private var model
    @State private var composerHeight: CGFloat = 0
    @State private var isAtBottom = true
    @State private var scrollToBottomRequest = 0
    @State private var presentedWidget: MountedWidget?
    @State private var showsChatAgentSettings = false

    var body: some View {
        @Bindable var model = model
        ZStack(alignment: .bottom) {
            TranscriptView(
                bottomInset: composerHeight,
                isAtBottom: $isAtBottom,
                scrollToBottomRequest: scrollToBottomRequest
            )
            .id(model.selectedSessionID)
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
        .navigationTitle(chatTitle)
        .toolbarTitleDisplayMode(.inline)
        .toolbar {
            // Title changes animate glyphs, so the principal title must be a view the app
            // owns rather than the system's opaque navigation title.
            ToolbarItem(placement: .principal) {
                VStack(spacing: HorusSpace.xxs) {
                    HorusTitleText(title: chatTitle)
                        .font(HorusStyle.titleFont)
                        .lineLimit(1)
                    if !chatSubtitle.isEmpty {
                        Text(chatSubtitle)
                            .font(HorusStyle.captionFont)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                .accessibilityElement(children: .combine)
            }
            // One group, so the two sit together rather than as separate items the bar
            // spaces apart. Files moved into the options menu, which already carries it.
            ToolbarItemGroup(placement: .primaryAction) {
                newChatButton
                ChatOptionsMenu(
                    presentedWidget: $presentedWidget,
                    showsAgentSettings: $showsChatAgentSettings
                )
            }
        }
        .sheet(item: $model.presentedPreview, content: PreviewTranscriptSheet.init)
        .sheet(item: $presentedWidget, content: FrontendWidgetSheet.init)
        .sheet(isPresented: $showsChatAgentSettings) {
            NavigationStack {
                AgentSettingsView(scope: .currentChat)
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Done") { showsChatAgentSettings = false }
                        }
                    }
            }
        }
    }

    /// Starting a chat in the folder you are already in belongs with the other page-level
    /// actions, not in the composer beside the controls that shape the message being written.
    private var newChatButton: some View {
        Button(action: model.openNewSessionInCurrentWorkspace) {
            toolbarGlyph(.notePencil)
        }
        .disabled(model.workspace == nil || !model.canCreateSession)
        .accessibilityLabel("New chat in this folder")
        .tint(.primary)
        .help("New chat in this folder")
    }

    /// A bare glyph is a 16pt target; toolbar buttons pad out to a full one the way every
    /// other icon button in the app does.
    private func toolbarGlyph(_ glyph: HorusGlyph) -> some View {
        HorusIcon(glyph, foreground: .primary)
            .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
            .contentShape(Rectangle())
    }

    private var workspaceName: String {
        guard let path = model.workspace?.path else { return "" }
        return path.split { $0 == "/" || $0 == "\\" }.last.map(String.init) ?? path
    }

    private var chatTitle: String {
        model.currentSessionTitle
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
    @Binding var showsAgentSettings: Bool

    var body: some View {
        Menu {
            Section(model.workspace?.path ?? "No chat selected") {
                if let git = model.gitStatus, !git.currentBranch.isEmpty {
                    Menu {
                        ForEach(git.branches, id: \.self) { branch in
                            Button {
                                model.switchGitBranch(to: branch)
                            } label: {
                                HorusLabel(
                                    title: branch,
                                    glyph: branch == git.currentBranch ? .check : .gitBranch
                                )
                            }
                            .disabled(branch == git.currentBranch)
                        }
                    } label: {
                        HorusLabel(
                            title: git.currentBranch,
                            glyph: .gitBranch
                        )
                    }
                    .disabled(model.isSwitchingGitBranch || !model.canModifySelectedSession)
                }
                Button { model.showFiles() } label: {
                    HorusLabel(
                        title: "Files",
                        glyph: .fileMagnifyingGlass
                    )
                }
                .disabled(model.selectedSessionID == nil || !model.connectionState.isReady)
                if let path = model.workspace?.path {
                    Button { copyToPasteboard(path) } label: {
                        HorusLabel(
                            title: "Copy workspace path",
                            glyph: .copy
                        )
                    }
                }
            }
            Section {
                Button {
                    showsAgentSettings = true
                } label: {
                    HorusLabel(
                        title: "Chat agent settings",
                        glyph: .slidersHorizontal
                    )
                }
                .disabled(model.selectedSessionID == nil || model.agentSnapshot == nil)
                ForEach(model.chatMenuWidgets) { widget in
                    Button {
                        activate(widget)
                    } label: {
                        HorusLabel(
                            title: widget.widget.text,
                            glyph: widget.glyph
                        )
                    }
                    .disabled(widget.widget.content == nil && widget.widget.action == nil)
                }
                Button {
                    model.startCronSetup()
                } label: {
                    HorusLabel(
                        title: "Schedule as a task…",
                        glyph: .calendarDots
                    )
                }
                .disabled(!model.canStartCronSetup)
                Button {
                    model.openWorkspaceBrowser()
                } label: {
                    HorusLabel(
                        title: "New chat in another folder…",
                        glyph: .folderPlus
                    )
                }
                .disabled(!model.canCreateSession)
            }
            if let session = model.selectedSession {
                Section {
                    Button {
                        model.beginRenamingSession(session)
                    } label: {
                        HorusLabel(title: "Rename chat", glyph: .pencilSimple)
                    }
                    .disabled(!model.canRenameSession)
                    Button(role: .destructive) {
                        model.beginDeletingSession(session)
                    } label: {
                        HorusLabel(title: "Delete chat", glyph: .trash)
                    }
                    .disabled(!model.canRenameSession)
                }
            }
        } label: {
            HorusIcon(.dotsThree)
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

/// One row of the transcript: a single entry, or a run of events that share one summary.
private struct TranscriptRowLayout: Identifiable {
    let entries: [TranscriptEntry]
    let topSpacing: CGFloat
    let isLast: Bool

    var id: String { entries.first?.id ?? "" }
    /// Activity always rides behind the group summary, even a run of one: the timeline is the
    /// narrative — the user, the agent's commentary, the answer — and everything else is a
    /// count you can open.
    var isEventGroup: Bool {
        entries.first?.kind.isActivity ?? false
    }
}

/// The waiting line's current state: when it started, and the order it rotates through.
///
/// It is a value rather than a view so the transcript can hand it to whichever row owns the
/// bottom slot — the tail group's header, or a line of its own when there is no group yet.
struct TranscriptWaitingPhrase: Equatable {
    let startedAt: Date
    let order: [String]
}

/// The transcript body shared by the full chat and read-only agent previews.
/// Navigation, pagination, and composing controls stay with their owning surface.
struct TranscriptRowsView: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let entries: [TranscriptEntry]
    var activeStepID: String?
    var breakBefore: String?
    var collapsesLongMessages = false
    var rowSpacing: CGFloat = 12
    /// The live transcript passes the model's cached grouping; smaller one-off transcripts
    /// (a preview sheet) group their own, where doing it per pass costs nothing.
    var groupedEntries: [[TranscriptEntry]]?
    /// Set when the transcript wants the last group's header to hold the waiting line.
    var waiting: TranscriptWaitingPhrase?

    var body: some View {
        ForEach(rows) { row in
            Group {
                if row.isEventGroup {
                    EventGroupView(
                        entries: row.entries,
                        isActive: row.entries.contains { $0.id == activeStepID },
                        waiting: row.isLast ? waiting : nil
                    )
                } else if let entry = row.entries.first {
                    TranscriptRow(
                        entry: entry,
                        isActive: entry.id == activeStepID,
                        collapsesLongMessages: collapsesLongMessages
                    )
                }
            }
            .id(row.id)
            .padding(.top, row.topSpacing)
            // A run of parallel tool calls arrives as one row with nothing before it, and a
            // plain fade leaves the transcript looking bumped. Rising the last few points into
            // place is the same entrance the waiting line makes, so the swap between them reads
            // as one thing continuing rather than two things happening.
            .transition(
                reduceMotion ? .opacity : .opacity.combined(with: .offset(y: 8))
            )
        }
        // A fast turn lands rows faster than the eye tracks them. One animation on the entry
        // count fades an arriving row in and glides the content above it, and covers a row
        // turning into a group. Streaming deltas leave the count alone, so text still grows
        // without a per-frame animation fighting the scroll anchor. The duration is the
        // waiting note's: the first activity row of a turn replaces it in place.
        .animation(
            reduceMotion ? nil : .easeInOut(duration: TranscriptWaitingNote.crossfade),
            value: entries.count
        )
    }

    private var rows: [TranscriptRowLayout] {
        let grouped = groupedEntries
            ?? TranscriptEntry.groupedRows(from: entries, breakBefore: breakBefore)
        return grouped.enumerated().map { index, entries in
            TranscriptRowLayout(
                entries: entries,
                topSpacing: index == 0 ? 0 : rowSpacing,
                isLast: index == grouped.count - 1
            )
        }
    }
}

struct TranscriptPaginationButton: View {
    @Environment(\.horusPalette) private var palette
    let isLoading: Bool
    let isEnabled: Bool
    let action: () -> Void

    var body: some View {
        HStack {
            Spacer()
            Button(action: action) {
                HorusLabel(
                    title: isLoading ? "Loading earlier messages" : "Load earlier messages",
                    glyph: .arrowUp,
                    iconColor: palette.accent
                )
                .frame(minHeight: HorusStyle.iconButtonSize)
            }
            .buttonStyle(.horusPlain)
            .foregroundStyle(isEnabled ? palette.accent : palette.muted)
            .tint(palette.accent)
            .disabled(!isEnabled)
            .accessibilityLabel(
                isLoading ? "Loading earlier messages" : "Load earlier messages"
            )
            Spacer()
        }
    }
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
    @State private var waitingSince: Date?
    @State private var waitingHold: Task<Void, Never>?
    @State private var waitingOrder = TranscriptWaitingNote.messages
    private let rowSpacing: CGFloat = 12
    private let contentPadding: CGFloat = 16

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
                    TranscriptPaginationButton(
                        isLoading: model.isLoadingEarlierHistory,
                        isEnabled: model.canLoadEarlierHistory,
                        action: loadEarlierHistory
                    )
                    .padding(.bottom, rowSpacing)
                }
                TranscriptRowsView(
                    entries: model.displayedTranscript,
                    activeStepID: model.activeTranscriptStepID,
                    breakBefore: historyAnchorID,
                    rowSpacing: rowSpacing,
                    groupedEntries: model.transcriptRows(breakBefore: historyAnchorID),
                    waiting: groupHoldsWaitingPhrase ? waitingPhrase : nil
                )
                ForEach(model.transcriptTailWidgets) { widget in
                    QueuedMessageView(widget: widget)
                        .padding(.top, rowSpacing)
                }
                if let waitingPhrase, !groupHoldsWaitingPhrase {
                    TranscriptWaitingNoteView(phrase: waitingPhrase, topSpacing: rowSpacing)
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
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom, for: .sizeChanges)
        .scrollIndicators(.hidden)
        .scrollDismissesKeyboard(.interactively)
        .refreshable { loadEarlierHistory() }
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
        }
        .onChange(of: model.selectedSessionID) { historyAnchorID = nil }
        .task(id: model.selectedSessionID) { await openAtLatest() }
        .onChange(of: model.isWaitingForModel, initial: true) { _, waiting in
            rescheduleWaitingPhrase(waiting)
        }
        .onDisappear {
            waitingHold?.cancel()
            waitingHold = nil
        }
    }

    private var waitingPhrase: TranscriptWaitingPhrase? {
        waitingSince.map { TranscriptWaitingPhrase(startedAt: $0, order: waitingOrder) }
    }

    /// Once the turn has a group at the tail, the gap between steps belongs to that row: the
    /// phrase takes over its summary rather than appearing below it and moving the transcript.
    private var groupHoldsWaitingPhrase: Bool {
        guard model.transcriptTailWidgets.isEmpty,
              model.displayedTranscript.last?.kind.isActivity == true,
              let lastGroup = model.transcriptRows(breakBefore: historyAnchorID).last
        else { return false }
        return lastGroup.contains { !$0.title.isEmpty || !$0.text.isEmpty }
    }

    /// Appearance is deliberately sticky: steps land a few hundred milliseconds apart in a busy
    /// turn, so the raw condition flickers several times a second and the phrase waits before
    /// showing, which is the difference between a status line and a strobe. It leaves without a
    /// delay, because what ends the wait always lands in the slot the phrase is holding.
    private func rescheduleWaitingPhrase(_ waiting: Bool) {
        waitingHold?.cancel()
        let fade = Animation.easeInOut(duration: TranscriptWaitingNote.crossfade)
        guard waiting else {
            withAnimation(fade) { waitingSince = nil }
            return
        }
        guard waitingSince == nil else { return }
        waitingHold = Task {
            try? await Task.sleep(for: .seconds(TranscriptWaitingNote.appearAfter))
            guard !Task.isCancelled else { return }
            waitingOrder.shuffle()
            withAnimation(fade) { waitingSince = Date() }
        }
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
        guard isAtBottom || (model.activeTurnID != nil && !position.isPositionedByUser) else { return }
        position.scrollTo(edge: .bottom)
    }

    private func loadEarlierHistory() {
        guard model.canLoadEarlierHistory else { return }
        historyAnchorID = model.displayedTranscript.first?.id
        model.loadEarlierHistory()
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
    let isActive: Bool
    var collapsesLongMessages = false

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
        case .assistant, .commentary:
            VStack(alignment: .leading, spacing: HorusSpace.s) {
                TranscriptFileCards(files: entry.files)
                if !entry.text.isEmpty {
                    if collapsesLongMessages {
                        CollapsibleText(
                            text: entry.text,
                            rendersMarkdown: true,
                            streaming: entry.pending
                        )
                    } else {
                        HorusMarkdownText(entry.text, streaming: entry.pending)
                            .equatable()
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            // Commentary arrives in one short burst between steps; the final message streams
            // long enough to read as it lands on its own.
            .horusStreamingReveal(
                count: entry.text.count,
                active: entry.kind == .commentary,
                live: model.isLiveTranscriptEntry(entry)
            )
        case .reasoning:
            ReasoningLine(entry: entry, isActive: isActive)
        case .event, .error:
            VStack(alignment: .leading, spacing: HorusSpace.s) {
                TranscriptFileCards(files: entry.files)
                if !entry.title.isEmpty || !entry.text.isEmpty {
                    EventLine(entry: entry, isActive: isActive)
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
                    .disabled(!model.canModifySelectedSession)
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
                    .disabled(!model.canModifySelectedSession)
                }
            }
        }
    }

    private func messageActionGlyph(_ widget: MountedWidget) -> HorusGlyph {
        widget.widget.symbol.map { HorusSymbol.glyph(for: $0) } ?? .dotsThree
    }

    private var hasMessageActions: Bool {
        entry.kind == .user || isAssistantMessage
    }

    private var hasInlineControls: Bool {
        isAssistantMessage
    }

    private var inlineControlsVisible: Bool {
        isAssistantMessage || isHovered
    }

    private var isAssistantMessage: Bool {
        entry.kind == .assistant || entry.kind == .commentary
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
        VStack(alignment: .trailing, spacing: HorusSpace.s) {
            TranscriptFileCards(files: entry.files)
            if !entry.text.isEmpty {
                CollapsibleText(text: entry.text)
                    .padding(.horizontal, HorusSpace.l)
                    .padding(.vertical, HorusSpace.m)
                    .background(palette.accentSoft, in: HorusStyle.cardShape)
            }
        }
    }
}

private struct CollapsibleTextEndAttribute: TextAttribute {}

struct CollapsibleText: View {
    private static let collapsedLineLimit = 21
    // Bound the text SwiftUI must shape while collapsed. Four thousand characters still
    // exceed 21 lines at the transcript's widest supported layout, including on iPad.
    private static let collapsedCharacterLimit = 4_096

    @Environment(\.horusPalette) private var palette
    @State private var isExpanded = false
    @State private var isTruncated = false
    @State private var hasMeasured = false
    let text: String
    var rendersMarkdown = false
    var streaming = false

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.s) {
            renderedText
            if isTruncated {
                Button(isExpanded ? "Show less" : "Read more") {
                    isExpanded.toggle()
                }
                .font(HorusStyle.captionFont.weight(.semibold))
                .foregroundStyle(palette.accent)
                .buttonStyle(.horusPlain)
                .frame(minHeight: HorusStyle.iconButtonSize, alignment: .leading)
                .accessibilityHint(
                    isExpanded ? "Collapses the message" : "Expands the full message"
                )
            }
        }
        .onChange(of: text) { _, _ in
            guard !isExpanded else { return }
            hasMeasured = false
            isTruncated = false
        }
    }

    @ViewBuilder
    private var renderedText: some View {
        if rendersMarkdown && (isExpanded || (hasMeasured && !isTruncated)) {
            HorusMarkdownText(text, streaming: streaming)
                .equatable()
        } else {
            markedText
                .lineLimit(isExpanded ? nil : Self.collapsedLineLimit)
                .truncationMode(.tail)
                .textSelection(.enabled)
                .onPreferenceChange(Text.LayoutKey.self, perform: measureTruncation)
        }
    }

    private func measureTruncation(_ layouts: Text.LayoutKey.Value) {
        guard !isExpanded, !layouts.isEmpty else { return }
        if hidesBoundedSuffix {
            isTruncated = true
            hasMeasured = true
            return
        }
        let reachedEnd = layouts.contains { proxy in
            proxy.layout.contains { line in
                line.contains { run in
                    run[CollapsibleTextEndAttribute.self] != nil
                }
            }
        }
        isTruncated = !reachedEnd
        hasMeasured = true
    }

    private var markedText: Text {
        let source = displayedText
        guard let end = source.lastIndex(where: { !$0.isNewline }) else {
            return Text(source)
        }
        return Text(
            "\(Text(source[..<end]))\(Text(source[end...]).customAttribute(CollapsibleTextEndAttribute()))"
        )
    }

    private var displayedText: String {
        guard !isExpanded else { return text }
        let prefix = text.prefix(Self.collapsedCharacterLimit)
        guard prefix.endIndex != text.endIndex else { return text }
        return "\(prefix)…"
    }

    private var hidesBoundedSuffix: Bool {
        text.prefix(Self.collapsedCharacterLimit).endIndex != text.endIndex
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
                HorusIcon(.dotsThree, size: HorusStyle.glyphInline)
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
            CollapsibleText(text: widget.widget.text)
                .font(HorusStyle.bodyFont)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, HorusSpace.l)
                .padding(.vertical, HorusSpace.m)
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
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Queued message")
        .accessibilityValue(editAction == nil ? "Queued" : "Queued, editable until sent")
        .accessibilityActions {
            if editAction != nil {
                Button("Edit queued message") { model.editWidgetInputInComposer(widget) }
            }
            Button("Copy queued message") { copyToPasteboard(widget.widget.text) }
        }
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
        .padding(HorusSpace.m)
        .frame(width: 136, height: 112)
        .background(palette.raised, in: HorusStyle.tileShape)
        .overlay(alignment: .topTrailing) { trailing.padding(HorusSpace.xs) }
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

/// A run of consecutive events behind one summary line, so a long turn costs one row until
/// the reader asks for more.
private struct EventGroupView: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isExpanded = false
    let entries: [TranscriptEntry]
    let isActive: Bool
    /// The gap between two steps belongs to this row: rather than growing the transcript by a
    /// line that then has to disappear again, the summary hands its slot to the waiting line.
    var waiting: TranscriptWaitingPhrase?

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.s) {
            // Files an event produced are the deliverable, not a detail, so they stay out.
            TranscriptFileCards(files: files)
            if !lines.isEmpty {
                Button {
                    withAnimation(.easeOut(duration: 0.16)) { isExpanded.toggle() }
                } label: {
                    header
                }
                .buttonStyle(.horusPlain)
                .accessibilityLabel(
                    waiting == nil ? TranscriptEntry.summary(for: lines) : "Waiting for the model"
                )
                .accessibilityHint(isExpanded ? "Collapses the steps" : "Expands the steps")
                if isExpanded {
                    VStack(alignment: .leading, spacing: HorusSpace.xxs) {
                        ForEach(lines) { entry in
                            if entry.kind == .reasoning {
                                ReasoningLine(entry: entry, isActive: false)
                            } else {
                                EventLine(entry: entry, isActive: false)
                            }
                        }
                    }
                }
            }
        }
    }

    private var header: some View {
        HStack(spacing: HorusSpace.s) {
            // The group keeps its own mark whether or not it is running: the summary beside
            // it shimmers while the run is live, so swapping in a spinner said the same
            // thing twice and cost the row its identity while it mattered most.
            HorusIcon(.group01, size: HorusStyle.glyphInline, foreground: palette.muted)
            Group {
                if let waiting {
                    TranscriptWaitingPhraseText(phrase: waiting)
                } else {
                    Text(TranscriptEntry.summary(for: lines))
                        .font(HorusStyle.metadataFont)
                        .foregroundStyle(palette.muted)
                        .lineLimit(1)
                        // The count climbs every time a step joins the run; morphing the digit
                        // reads as the same line counting up rather than a new line replacing it.
                        .contentTransition(.numericText())
                        // The group is one transcript step, so its summary owns the running mark.
                        .horusRunningShimmer(active: isActive)
                }
            }
            .transition(.opacity)
            .animation(
                reduceMotion ? nil : .easeInOut(duration: TranscriptWaitingNote.crossfade),
                value: waiting != nil
            )
            Spacer(minLength: HorusSpace.s)
            HorusIcon(.caretUpDown, size: HorusStyle.glyphMark, foreground: palette.muted)
        }
        .frame(minHeight: HorusStyle.rowRegular)
        .contentShape(Rectangle())
    }

    private var lines: [TranscriptEntry] {
        entries.filter { !$0.title.isEmpty || !$0.text.isEmpty }
    }

    /// Two events in a run can carry the same file, and `ForEach` needs the ids unique.
    private var files: [SessionFileReference] {
        var seen = Set<String>()
        return entries.flatMap(\.files).filter { seen.insert($0.id).inserted }
    }
}

/// Reasoning is its own disclosure: the first row is the summary and expands in place.
private struct ReasoningLine: View {
    private static let summaryCharacterLimit = 512

    @Environment(\.horusPalette) private var palette
    @State private var isExpanded = false
    let entry: TranscriptEntry
    let isActive: Bool

    var body: some View {
        Button {
            withAnimation(.easeOut(duration: 0.16)) { isExpanded.toggle() }
        } label: {
            // A glyph has no baseline, so `.firstTextBaseline` hung this one by its bottom
            // edge and left it sitting low. Centred while the summary is one line, topped
            // once the reasoning expands into a block.
            HStack(alignment: isExpanded ? .top : .center, spacing: HorusSpace.s) {
                HorusIcon(.setup01, size: HorusStyle.glyphInline, foreground: palette.muted)
                Group {
                    if isExpanded {
                        HorusMarkdownText(entry.text, streaming: entry.pending)
                            .equatable()
                    } else {
                        Text(summary)
                    }
                }
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                    .multilineTextAlignment(.leading)
                    .lineLimit(isExpanded ? nil : 1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .allowsHitTesting(false)
                    // The transcript owns which phase is current; an older reasoning stream
                    // can remain pending while a later tool call is already running.
                    .horusRunningShimmer(active: isActive && !isExpanded)
            }
            .frame(minHeight: HorusStyle.rowCompact)
            .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .accessibilityLabel(entry.text)
        .accessibilityHint(isExpanded ? "Collapses the reasoning" : "Expands the reasoning")
    }

    private var summary: AttributedString {
        let lineEnd = entry.text.firstIndex(of: "\n") ?? entry.text.endIndex
        let line = entry.text[..<lineEnd]
        let end = line.index(
            line.startIndex,
            offsetBy: Self.summaryCharacterLimit,
            limitedBy: line.endIndex
        ) ?? line.endIndex
        let source = String(line[..<end])
        var summary = (try? AttributedString(markdown: source)) ?? AttributedString(source)
        if end != line.endIndex || lineEnd != entry.text.endIndex {
            summary.append(AttributedString("…"))
        }
        return summary
    }
}

/// The rotating waiting line, wherever it is shown.
private struct TranscriptWaitingPhraseText: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let phrase: TranscriptWaitingPhrase

    var body: some View {
        // The clock drives the rotation, so a transcript rebuild cannot restart it and the
        // message advances on its own schedule rather than on redraws.
        TimelineView(.periodic(from: phrase.startedAt, by: TranscriptWaitingNote.rotation)) { context in
            let elapsed = reduceMotion ? 0 : context.date.timeIntervalSince(phrase.startedAt)
            Text(TranscriptWaitingNote.message(in: phrase.order, elapsed: elapsed))
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
                .lineLimit(1)
                .truncationMode(.tail)
                .contentTransition(.opacity)
                .animation(.easeInOut(duration: 0.3), value: elapsed)
                .horusRunningShimmer(active: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        // One stable label: rotating the joke past VoiceOver every few seconds is noise.
        .accessibilityElement()
        .accessibilityLabel("Waiting for the model")
    }
}

/// The waiting line on a row of its own, for the part of a turn that has no group yet.
private struct TranscriptWaitingNoteView: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let phrase: TranscriptWaitingPhrase
    /// Owned rather than applied by the transcript: padding outside the condition reserves
    /// the gap while the note is hidden, and the arriving row then lands 12pt low.
    let topSpacing: CGFloat

    var body: some View {
        HStack(spacing: HorusSpace.s) {
            HorusIcon(.neuralNetwork, size: HorusStyle.glyphInline, foreground: palette.muted)
            TranscriptWaitingPhraseText(phrase: phrase)
        }
        // The group header's height, so the row that replaces this one lands on the same
        // baseline and the swap reads as one line changing rather than two rows trading places.
        .frame(minHeight: HorusStyle.rowRegular)
        .padding(.top, topSpacing)
        .transition(
            reduceMotion ? .opacity : .opacity.combined(with: .offset(y: 8))
        )
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Waiting for the model")
    }
}

/// One typed event on one line: its semantic owner, title, and optional detail.
private struct EventLine: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var isExpanded = false
    let entry: TranscriptEntry
    let isActive: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.xs) {
            if isInteractive {
                Button(action: activate) { line }
                    .buttonStyle(.horusPlain)
                    .accessibilityLabel("\(middlewareLabel), \(headline)")
                    .accessibilityValue(isExpanded ? "Expanded" : "Collapsed")
                    .accessibilityHint(accessibilityHint)
            } else {
                line
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("\(middlewareLabel), \(headline)")
            }
            if isExpanded {
                if entry.format == "unified_diff" {
                    InlineUnifiedDiffView(source: entry.text)
                } else if !entry.eventDetail.isEmpty {
                    Text(entry.eventDetail)
                        .font(HorusStyle.metadataFont)
                        .foregroundStyle(palette.muted)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, HorusSpace.m)
                        .padding(.vertical, HorusSpace.s)
                        .background(palette.panel, in: HorusStyle.controlShape)
                }
            }
        }
    }

    private func activate() {
        withAnimation(.easeOut(duration: 0.16)) { isExpanded.toggle() }
    }

    private var line: some View {
        HStack(spacing: HorusSpace.s) {
            HorusIcon(glyph, size: HorusStyle.glyphInline, foreground: headlineColor)
            HStack(spacing: HorusSpace.s) {
                Text(middlewareLabel)
                    .foregroundStyle(palette.accent)
                Text("•")
                    .foregroundStyle(palette.muted)
                Text(headline)
                    .foregroundStyle(headlineColor)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .horusRunningShimmer(active: isActive)
            Spacer(minLength: HorusSpace.s)
            // No spinner: the shimmer already says this step is running, and two marks for
            // one fact left the trailing slot flickering between them as steps completed.
            if entry.format == "unified_diff" {
                HorusIcon(.caretRight, size: HorusStyle.glyphMark, foreground: palette.muted)
                    .rotationEffect(.degrees(isExpanded ? 90 : 0))
                    .animation(.snappy(duration: 0.18), value: isExpanded)
            } else if !entry.eventDetail.isEmpty {
                HorusIcon(.caretUpDown, size: HorusStyle.glyphMark, foreground: palette.muted)
            }
        }
        .font(HorusStyle.metadataFont)
        .frame(minHeight: HorusStyle.rowCompact)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    /// A diff says more as a count of changed lines than as the word "Code change".
    private var headline: String {
        entry.format == "unified_diff" ? diffSummary(entry.text) : entry.headline
    }

    private var glyph: HorusGlyph {
        if entry.kind == .error || entry.tone == "error" { return .xCircle }
        if entry.format == "unified_diff" { return .fileMagnifyingGlass }
        if let symbol = entry.symbol, let glyph = HorusSymbol.knownGlyph(for: symbol) {
            return glyph
        }
        return switch entry.role {
        case .webSearch: .globe02
        case .artifact: .fileMagnifyingGlass
        case .approval: .checkCircle
        case .activity, .tool, .notice, nil: .typeCursor
        }
    }

    private var middlewareLabel: String {
        guard let capability = entry.capability else { return "Event" }
        if let feature = model.middlewareFeatures.first(where: { $0.id == capability }) {
            return feature.label
        }
        return capability.replacingOccurrences(of: "_", with: " ").capitalized
    }

    private var headlineColor: Color {
        entry.tone == "neutral" ? .primary : palette.tone(entry.tone)
    }

    private var isInteractive: Bool {
        entry.format == "unified_diff" || !entry.eventDetail.isEmpty
    }

    private var accessibilityHint: String {
        if entry.format == "unified_diff" {
            return isExpanded ? "Collapses code changes" : "Shows code changes"
        }
        return isExpanded ? "Collapses details" : "Expands details"
    }
}

/// Equatable so an unchanged message is skipped entirely.
///
/// Text and streaming are the view's whole input, so comparing them is complete rather than
/// a guess. Without this, every row's body re-runs whenever anything in the transcript
/// changes: each one rescans its own text for `\dots` and rebuilds the markdown subtree,
/// which during streaming is a few hundred messages of work per frame to redraw one.
private struct HorusMarkdownText: View, Equatable {
    let text: String
    let streaming: Bool

    init(_ text: String, streaming: Bool) {
        self.text = text
        self.streaming = streaming
    }

    var body: some View {
        StreamingMarkdown(text: normalizedText, streaming: streaming)
            .markdownFontGroup(HorusMarkdownFonts())
            .markdownMathRenderingEnabled()
            .markdownTableStyle(.github)
            .markdownBlockQuoteStyle(.github)
            .markdownCodeBlockStyle(.default(lightTheme: "xcode", darkTheme: "dark"))
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var normalizedText: String {
        guard text.contains(#"\dots"#) else { return text }
        return text.replacingOccurrences(
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
    let actionsEnabled: Bool
    let select: (FrontendPickerOption) -> Void

    init(
        content: FrontendWidgetContent,
        actionsEnabled: Bool = true,
        select: @escaping (FrontendPickerOption) -> Void
    ) {
        self.content = content
        self.actionsEnabled = actionsEnabled
        self.select = select
    }

    var body: some View {
        switch content {
        case .blocks(_, let blocks):
            ForEach(blocks) { block in
                PreviewBlockView(block: block.block)
                    .padding(.vertical, HorusSpace.s)
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
                .accessibilityValue(option.showsDetail ? option.detail : option.description)
                .accessibilityHint(
                    option.showsDetail ? option.description : "Activates this option"
                )
                .disabled(!actionsEnabled)
            }
        case .actionList(_, let items):
            if items.isEmpty {
                Text("Nothing here yet.")
                    .foregroundStyle(palette.muted)
                    .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize)
            } else {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(items) { item in
                        FrontendActionListRow(item: item, actionsEnabled: actionsEnabled)
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
    let actionsEnabled: Bool

    var body: some View {
        HStack(alignment: .top, spacing: HorusSpace.s) {
            if let statusGlyph {
                HorusIcon(statusGlyph, size: HorusStyle.glyphInline, foreground: statusColor)
                    .frame(height: HorusStyle.rowTouch)
            }
            Text(item.text)
                .font(HorusStyle.bodyFont)
                .foregroundStyle(item.state == .completed ? palette.muted : .primary)
                .strikethrough(item.state == .completed, color: palette.muted)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize, alignment: .leading)
            if !item.actions.isEmpty {
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
                .disabled(!actionsEnabled)
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
                    !actionsEnabled
                        || editedText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || editedText == pending.action.op.capabilityInput
                )
            case .destructive:
                Button("Cancel", role: .cancel) { pendingAction = nil }
                Button(pending.action.label, role: .destructive) {
                    model.submitFrontendOperation(pending.action.op)
                    pendingAction = nil
                }
                .disabled(!actionsEnabled)
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
        guard actionsEnabled else { return }
        if action.tone == "error" {
            pendingAction = PendingAction(kind: .destructive, itemText: item.text, action: action)
        } else if let input = action.op.capabilityInput {
            editedText = input
            pendingAction = PendingAction(kind: .edit, itemText: item.text, action: action)
        } else {
            model.submitFrontendOperation(action.op)
        }
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
        HStack(spacing: HorusSpace.s) {
            if let symbol = option.symbol,
               let glyph = HorusSymbol.knownGlyph(for: symbol) {
                HorusIcon(glyph, size: HorusStyle.glyphInline, foreground: palette.accent)
            }
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
            Spacer(minLength: HorusSpace.xs)
            if option.showsDetail, !option.detail.isEmpty {
                Text(option.detail)
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
                    .lineLimit(1)
            }
            HorusIcon(.caretRight, size: HorusStyle.glyphMark, foreground: palette.muted)
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
                if !model.isCapabilityEnabled(widget.capability) {
                    DisabledCapabilityNotice(
                        title: "\(currentWidget?.widget.text ?? widget.widget.text) is off",
                        detail: "Saved content remains visible. Enable \(currentWidget?.widget.text ?? widget.widget.text) in this chat to make changes."
                    )
                }
                if let content = currentWidget?.widget.content {
                    Section {
                        FrontendWidgetContentView(
                            content: content,
                            actionsEnabled: model.isCapabilityEnabled(widget.capability)
                        ) { option in
                            model.submitPickerOption(option)
                            dismiss()
                        }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .navigationTitle(currentWidget?.title ?? widget.title)
            .toolbarTitleDisplayMode(.inline)
            .background(HorusBackdrop())
        }
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
            VStack(alignment: .leading, spacing: HorusSpace.m) {
                HStack {
                    Text(picker.title)
                        .font(HorusStyle.titleFont)
                    Spacer(minLength: HorusSpace.s)
                    Button { model.pendingPicker = nil } label: {
                        HorusIcon(.x, size: HorusStyle.glyphInline, foreground: palette.muted)
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
                // A full agent list must scroll instead of growing the card off screen.
                ScrollView {
                    VStack(alignment: .leading, spacing: HorusSpace.m) {
                        ForEach(picker.options) { option in
                            Button { model.submitPickerOption(option) } label: {
                                FrontendPickerOptionLabel(option: option)
                            }
                            .buttonStyle(.horusPlain)
                            .accessibilityLabel(option.label)
                            .accessibilityValue(option.showsDetail ? option.detail : option.description)
                            .accessibilityHint(
                                option.showsDetail ? option.description : "Activates this option"
                            )
                        }
                    }
                }
                .frame(maxHeight: HorusStyle.rowTouch * 8)
                .scrollBounceBehavior(.basedOnSize)
            }
        }
    }
}

private func copyToPasteboard(_ text: String) {
    UIPasteboard.general.string = text
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
