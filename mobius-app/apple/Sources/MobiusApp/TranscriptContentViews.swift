import Foundation
import SwiftUI

/// Files sit above the bubble rather than inside it: nesting a bordered card in a
/// filled bubble reads as a box in a box, and the pill carries the same fill so the pair
/// still reads as one message.
struct UserMessageContent: View {
    @Environment(\.mobiusPalette) private var palette
    let entry: TranscriptEntry

    var body: some View {
        VStack(alignment: .trailing, spacing: MobiusSpace.s) {
            TranscriptFileCards(files: entry.files)
            if !entry.text.isEmpty {
                CollapsibleText(text: entry.text)
                    .padding(.horizontal, MobiusSpace.l)
                    .padding(.vertical, MobiusSpace.m)
                    .background(palette.accentSoft, in: MobiusStyle.cardShape)
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

    @Environment(\.mobiusPalette) private var palette
    @State private var isExpanded = false
    @State private var isTruncated = false
    @State private var hasMeasured = false
    let text: String
    var rendersMarkdown = false
    var streaming = false

    var body: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.s) {
            renderedText
            if isTruncated {
                Button(isExpanded ? "Show less" : "Read more") {
                    isExpanded.toggle()
                }
                .font(MobiusStyle.captionFont.weight(.semibold))
                .foregroundStyle(palette.accent)
                .buttonStyle(.mobiusPlain)
                .frame(minHeight: MobiusStyle.iconButtonSize, alignment: .leading)
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
            MobiusMarkdownText(text, streaming: streaming)
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

struct TranscriptFileCards: View {
    let files: [SessionFileReference]

    var body: some View {
        ForEach(files) { file in
            SessionFileCard(file: file)
        }
    }
}

struct TurnDiffCard: View {
    @Environment(\.mobiusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var document: UnifiedDiffDocument?
    @State private var isExpanded = false
    @State private var showsDetails = false
    let source: String

    var body: some View {
        VStack(spacing: 0) {
            if let document, !document.files.isEmpty {
                card(document)
            }
        }
        .task(id: source) {
            document = nil
            let parseTask = Task.detached(priority: .userInitiated) {
                UnifiedDiffDocument(source)
            }
            let parsed = await withTaskCancellationHandler {
                await parseTask.value
            } onCancel: {
                parseTask.cancel()
            }
            guard !Task.isCancelled else { return }
            document = parsed
        }
        .sheet(isPresented: $showsDetails) {
            NavigationStack {
                ZStack {
                    MobiusBackdrop()
                    WorkspaceDiffView(
                        source: source,
                        revision: 0,
                        isLoading: false,
                        title: "changes from this turn"
                    )
                }
                .navigationTitle("Turn changes")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done") { showsDetails = false }
                    }
                }
            }
            .presentationDetents([.medium, .large])
            .presentationDragIndicator(.visible)
        }
    }

    private func card(_ document: UnifiedDiffDocument) -> some View {
        let files = document.fileChanges
        return VStack(spacing: 0) {
            Button {
                withAnimation(reduceMotion ? nil : .easeOut(duration: 0.16)) {
                    isExpanded.toggle()
                }
            } label: {
                HStack(spacing: MobiusSpace.s) {
                    Text("\(files.count) file\(files.count == 1 ? "" : "s") changed")
                        .foregroundStyle(.primary)
                    Text("+\(document.added)")
                        .foregroundStyle(palette.signal)
                    Text("−\(document.removed)")
                        .foregroundStyle(palette.danger)
                    Spacer(minLength: MobiusSpace.s)
                    MobiusIcon(.caretRight, size: MobiusStyle.glyphInline, foreground: palette.muted)
                        .rotationEffect(.degrees(isExpanded ? 90 : 0))
                }
                .font(MobiusStyle.badgeFont)
                .padding(.horizontal, MobiusSpace.l)
                .frame(minHeight: MobiusStyle.rowTouch)
                .contentShape(Rectangle())
            }
            .buttonStyle(.mobiusPlain)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(
                "\(files.count) file\(files.count == 1 ? "" : "s") changed, "
                    + "\(document.added) additions, "
                    + "\(document.removed) removals"
            )
            .accessibilityValue(isExpanded ? "Expanded" : "Collapsed")
            .accessibilityHint(isExpanded ? "Collapses the file list" : "Shows the file list")

            if isExpanded {
                ForEach(files.prefix(3)) { file in
                    fileRow(file)
                }
                Button {
                    showsDetails = true
                } label: {
                    HStack(spacing: MobiusSpace.s) {
                        Text(detailsTitle(files.count))
                            .foregroundStyle(palette.muted)
                        Spacer(minLength: MobiusSpace.s)
                        MobiusIcon(
                            .caretRight,
                            size: MobiusStyle.glyphInline,
                            foreground: palette.muted
                        )
                    }
                    .padding(.horizontal, MobiusSpace.l)
                    .frame(minHeight: MobiusStyle.rowTouch)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.mobiusPlain)
                .accessibilityHint("Opens the full diff")
            }
        }
        .mobiusGlass(in: MobiusStyle.cardShape)
    }

    private func fileRow(_ file: UnifiedDiffFileChange) -> some View {
        HStack(spacing: MobiusSpace.s) {
            Text(file.path)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: MobiusSpace.s)
            Text("+\(file.added)").foregroundStyle(palette.signal)
            Text("−\(file.removed)").foregroundStyle(palette.danger)
        }
        .font(MobiusStyle.metadataFont)
        .padding(.horizontal, MobiusSpace.l)
        .frame(minHeight: MobiusStyle.rowRegular)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "File \(file.path), \(file.added) additions, \(file.removed) removals"
        )
    }

    private func detailsTitle(_ fileCount: Int) -> String {
        let remaining = fileCount - 3
        return remaining > 0
            ? "View \(remaining) more file\(remaining == 1 ? "" : "s")"
            : "View all changes"
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
            .buttonStyle(.mobiusPlain)
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
                MobiusIcon(.dotsThree, size: MobiusStyle.glyphInline)
                    .frame(width: MobiusStyle.iconButtonSize, height: MobiusStyle.iconButtonSize)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.mobiusPlain)
            .disabled(model.isLoadingFilePresentation)
            .accessibilityLabel("File actions for \(file.name)")
            .help("File actions")
        }
    }
}

struct QueuedMessageView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette
    let widget: MountedWidget

    var body: some View {
        HStack {
            Spacer(minLength: 42)
            CollapsibleText(text: widget.widget.text)
                .font(MobiusStyle.bodyFont)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, MobiusSpace.l)
                .padding(.vertical, MobiusSpace.m)
                .background(palette.accentSoft.opacity(0.24), in: MobiusStyle.cardShape)
                .overlay {
                    MobiusStyle.cardShape.stroke(
                        palette.accent.opacity(0.42),
                        style: StrokeStyle(lineWidth: 1.25, lineCap: .round, dash: [1, 4])
                    )
                }
                .contentShape(MobiusStyle.cardShape)
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
    @Environment(\.mobiusPalette) private var palette
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
struct FileCard<Trailing: View>: View {
    @Environment(\.mobiusPalette) private var palette
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
            MobiusIcon(.fileText, size: 26, foreground: palette.accent)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            Text(name)
                .font(MobiusStyle.badgeFont)
                .lineLimit(1)
                .truncationMode(.middle)
            detail
                .font(MobiusStyle.badgeFont)
                .foregroundStyle(detailColor)
                .lineLimit(1)
        }
        .padding(MobiusSpace.m)
        .frame(width: 136, height: 112)
        .background(palette.raised, in: MobiusStyle.tileShape)
        .overlay(alignment: .topTrailing) { trailing.padding(MobiusSpace.xs) }
        .contentShape(MobiusStyle.tileShape)
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

struct MessageActionButton: View {
    @Environment(\.mobiusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isHovered = false
    let title: String
    let glyph: MobiusGlyph
    let action: () -> Void

    var body: some View {
        // Secondary actions, so a smaller glyph in a smaller box than a standalone icon button:
        // the box is what spaces these apart, and the context menu carries the same actions.
        Button(action: action) {
            ZStack {
                MobiusIcon(
                    glyph,
                    size: 13,
                    foreground: isHovered ? palette.accent : palette.muted
                )
                .id(glyph)
                .transition(.scale(scale: 0.7).combined(with: .opacity))
            }
            .frame(width: 26, height: 26)
            .contentShape(Rectangle())
            .animation(reduceMotion ? nil : .snappy(duration: 0.18), value: glyph)
        }
        .buttonStyle(.mobiusPlain)
        .onHover { isHovered = $0 }
        .animation(.easeOut(duration: 0.12), value: isHovered)
        .accessibilityLabel(title)
        .help(title)
    }
}
