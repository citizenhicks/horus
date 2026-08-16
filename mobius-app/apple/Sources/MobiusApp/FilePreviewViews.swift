import Foundation
import SwiftUI
import HighlightSwift
import UIKit

struct InspectorLoadingView: View {
    let title: String

    var body: some View {
        ProgressView(title)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityLabel(title)
    }
}

struct TextFilePreviewView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.mobiusPalette) private var palette
    let preview: TextFilePreview

    var body: some View {
        NavigationStack {
            NumberedSourceText(
                preview.contents,
                language: preview.name.sourceHighlightLanguage
            )
                .background(palette.canvas)
                .navigationTitle(preview.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done", action: dismiss.callAsFunction)
                }
            }
        }
        .presentationDetents([.large])
    }
}

struct SessionFileShareView: UIViewControllerRepresentable {
    let file: SessionFileShareItem

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: [file.url], applicationActivities: nil)
    }

    func updateUIViewController(_ viewController: UIActivityViewController, context: Context) {}
}

struct NumberedSourceLine: Identifiable, Sendable {
    let id: Int
    let text: AttributedString
}

private struct NumberedSourceRenderRequest: Equatable, Sendable {
    let source: String
    let language: HighlightLanguage?
    let isDark: Bool
}

struct NumberedSourceText: View {
    @Environment(\.colorScheme) private var colorScheme
    let source: String
    let language: HighlightLanguage?
    @State private var lines: [NumberedSourceLine] = []

    init(_ source: String, language: HighlightLanguage? = nil) {
        self.source = source
        self.language = language
    }

    var body: some View {
        ScrollView(.vertical) {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(lines) { line in
                    HStack(alignment: .top, spacing: 0) {
                        Text(String(line.id))
                            .font(MobiusStyle.metadataFont)
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                            .frame(width: 44, alignment: .trailing)
                            .padding(.trailing, MobiusSpace.m)
                        Text(line.text.characters.isEmpty ? AttributedString(" ") : line.text)
                            .font(MobiusStyle.metadataFont)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, MobiusSpace.l)
            .padding(.trailing, MobiusSpace.l)
        }
        .textSelection(.enabled)
        .overlay {
            if lines.isEmpty, !source.isEmpty {
                ProgressView("Rendering text")
            }
        }
        .task(id: renderRequest) {
            lines = []

            let request = renderRequest
            let plainTask = Task.detached(priority: .userInitiated) {
                Self.lines(from: AttributedString(request.source))
            }
            let plainLines = await plainTask.value
            guard !Task.isCancelled else { return }
            lines = plainLines

            let highlightTask = Task.detached(priority: .userInitiated) {
                guard !Task.isCancelled else { return Optional<[NumberedSourceLine]>.none }
                let colors: HighlightColors = request.isDark ? .dark(.xcode) : .light(.xcode)
                let mode = request.language.map(HighlightMode.language) ?? .automatic
                guard let result = try? await Highlight().request(
                    request.source,
                    mode: mode,
                    colors: colors
                ), !Task.isCancelled else { return nil }
                let text = Self.restoringWhitespace(result.attributedText, in: request.source)
                return Self.lines(from: text)
            }
            let highlightedLines = await withTaskCancellationHandler {
                await highlightTask.value
            } onCancel: {
                highlightTask.cancel()
            }
            guard let highlightedLines, !Task.isCancelled else { return }
            lines = highlightedLines
        }
    }

    private var renderRequest: NumberedSourceRenderRequest {
        NumberedSourceRenderRequest(
            source: source,
            language: language,
            isDark: colorScheme == .dark
        )
    }

    nonisolated static func restoringWhitespace(
        _ highlighted: AttributedString,
        in source: String
    ) -> AttributedString {
        let trimmed = source.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              String(highlighted.characters) == trimmed,
              let range = source.range(of: trimmed)
        else { return AttributedString(source) }
        var result = AttributedString(String(source[..<range.lowerBound]))
        result.append(highlighted)
        result.append(AttributedString(String(source[range.upperBound...])))
        return result
    }

    nonisolated static func lines(from text: AttributedString) -> [NumberedSourceLine] {
        var lines: [AttributedString] = []
        var start = text.startIndex
        while let newline = text.characters[start...].firstIndex(where: \.isNewline) {
            lines.append(AttributedString(text[start..<newline]))
            start = text.characters.index(after: newline)
        }
        lines.append(AttributedString(text[start..<text.endIndex]))
        return lines.enumerated().map { NumberedSourceLine(id: $0.offset + 1, text: $0.element) }
    }
}

struct PreviewTranscriptSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette
    @State private var retainedEntryID: String?
    @State private var selectedDetent: PresentationDetent = .large
    let preview: TranscriptPreview

    var body: some View {
        VStack(spacing: 0) {
            header
            ZStack {
                ScrollViewReader { proxy in
                    ScrollView {
                        // This is the chat transcript surface without its navigation or composer.
                        // Exact rows avoid lazy height estimates for very long agent messages.
                        VStack(alignment: .leading, spacing: 0) {
                            if currentPreview.next != nil {
                                TranscriptPaginationButton(
                                    isLoading: model.isLoadingPreviewPage,
                                    isEnabled: !model.isLoadingPreviewPage
                                ) { loadEarlierPage() }
                                .padding(.bottom, MobiusSpace.m)
                            }
                            TranscriptRowsView(
                                projection: projection,
                                collapsesLongMessages: true
                            )
                        }
                        .scrollTargetLayout()
                        .frame(maxWidth: 880)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(MobiusSpace.l)
                    }
                    .scrollIndicators(.hidden)
                    .refreshable { loadEarlierPage() }
                    .onChange(of: model.isLoadingPreviewPage) { _, loading in
                        guard !loading, let retainedEntryID else { return }
                        let row = projection.rows.first { row in
                            row.id == retainedEntryID
                                || row.records.contains {
                                    $0.presentationID == retainedEntryID
                                }
                        }
                        if let row { proxy.scrollTo(row.id, anchor: .top) }
                        self.retainedEntryID = nil
                    }
                }

                if model.isLoadingPreviewPage {
                    ZStack {
                        palette.canvas.opacity(0.58)
                        MobiusComposingOrb()
                            .frame(width: 112, height: 112)
                    }
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("Loading earlier agent messages")
                }
            }
        }
        .background(MobiusBackdrop())
        .presentationDetents([.medium, .large], selection: $selectedDetent)
    }

    private var header: some View {
        HStack(spacing: MobiusSpace.s) {
            Text(agentName)
                .font(MobiusStyle.controlFont.weight(.semibold))
                .lineLimit(1)
                .truncationMode(.middle)
            if let status = currentPreview.status {
                headerSeparator
                Text(status)
                    .font(MobiusStyle.metadataFont)
                    .foregroundStyle(status == "errored" ? palette.danger : palette.muted)
                    .lineLimit(1)
            }
            if let choice = modelChoice {
                headerSeparator
                MobiusMenuLabel(
                    text: model.modelLabel(for: choice),
                    glyph: model.providerSymbol(for: choice)
                        .flatMap(MobiusSymbol.knownGlyph(for:)) ?? .robot,
                    detail: choice.reasoningEffort?.capitalized,
                    showsDisclosure: false
                )
                .layoutPriority(1)
                .accessibilityLabel("Model and reasoning")
                .accessibilityValue(modelSummary(choice))
            }
            Spacer(minLength: 0)
            if !currentPreview.context.isEmpty {
                SettingsInfoButton(
                    title: "Spawn context: \(currentPreview.context)",
                    detail: spawnContextDetail,
                    glyph: spawnContextGlyph,
                    accessibilityHint: "Explains the inherited conversation context"
                )
            }
        }
        .frame(maxWidth: .infinity, minHeight: MobiusStyle.iconButtonSize, alignment: .leading)
        .padding(.leading, MobiusSpace.l)
        .padding(.trailing, MobiusStyle.iconRowPadding)
        .padding(.vertical, MobiusSpace.s)
        .accessibilityElement(children: .contain)
    }

    private var headerSeparator: some View {
        Text("•")
            .font(MobiusStyle.metadataFont)
            .foregroundStyle(palette.muted)
            .accessibilityHidden(true)
    }

    private func loadEarlierPage() {
        guard let next = currentPreview.next, !model.isLoadingPreviewPage else { return }
        retainedEntryID = currentPreview.entries.first?.presentationID
        model.loadPreviewPage(next)
    }

    private var currentPreview: TranscriptPreview {
        if model.presentedPreview?.id == preview.id, let presented = model.presentedPreview {
            return presented
        }
        return model.previews.first(where: { $0.id == preview.id }) ?? preview
    }

    private var projection: TranscriptProjection {
        TranscriptProjection(
            entries: currentPreview.entries,
            breakBefore: retainedEntryID
        )
    }

    private var agentName: String {
        currentPreview.title
    }

    private var modelChoice: ModelChoice? {
        guard let route = currentPreview.model else { return nil }
        return model.modelChoices.first { $0.route == route }
    }

    private func modelSummary(_ choice: ModelChoice) -> String {
        let name = model.modelLabel(for: choice)
        guard let reasoning = choice.reasoningEffort, !reasoning.isEmpty else { return name }
        return "\(name) · \(reasoning.capitalized)"
    }

    private var spawnContextGlyph: MobiusGlyph {
        let context = currentPreview.context.lowercased()
        if context.hasPrefix("no ") || context == "none" { return .circle }
        if context.hasPrefix("full") { return .circleDot }
        return .circleDotDashed
    }

    private var spawnContextDetail: String {
        let context = currentPreview.context.lowercased()
        if context.hasPrefix("no ") || context == "none" {
            return "This agent started fresh with only its assigned task. It inherited none of the parent conversation."
        }
        if context.hasPrefix("full") {
            return "This agent inherited the full parent conversation as its starting context."
        }
        return "This agent inherited \(currentPreview.context.lowercased()) from the parent conversation."
    }
}

struct PreviewBlockView: View {
    @Environment(\.mobiusPalette) private var palette
    let block: FrontendBlock

    var body: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.s) {
            ForEach(block.files) { file in
                SessionFileCard(file: file)
            }
            if !block.text.isEmpty {
                HStack(alignment: .top, spacing: MobiusSpace.s) {
                    if block.pending { ProgressView().controlSize(.mini) }
                    CollapsibleText(text: block.text)
                        .font(
                            block.format == "unified_diff"
                                ? MobiusStyle.metadataFont
                                : MobiusStyle.bodyFont
                        )
                        .foregroundStyle(
                            block.tone == "neutral" ? Color.primary : palette.tone(block.tone)
                        )
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }
}
