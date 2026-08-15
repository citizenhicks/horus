import Foundation
import SwiftUI

/// Files sit above the bubble rather than inside it: nesting a bordered card in a
/// filled bubble reads as a box in a box, and the pill carries the same fill so the pair
/// still reads as one message.
struct UserMessageContent: View {
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

struct TranscriptFileCards: View {
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

struct QueuedMessageView: View {
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
struct FileCard<Trailing: View>: View {
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

struct MessageActionButton: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isHovered = false
    let title: String
    let glyph: HorusGlyph
    let action: () -> Void

    var body: some View {
        // Secondary actions, so a smaller glyph in a smaller box than a standalone icon button:
        // the box is what spaces these apart, and the context menu carries the same actions.
        Button(action: action) {
            ZStack {
                HorusIcon(
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
        .buttonStyle(.horusPlain)
        .onHover { isHovered = $0 }
        .animation(.easeOut(duration: 0.12), value: isHovered)
        .accessibilityLabel(title)
        .help(title)
    }
}
