import SwiftUI
import HighlightSwift
import UIKit

struct FilesView: View {
    @Environment(AppModel.self) private var model

    // A NavigationStack for the title and, more to the point, for `.searchable`: the search
    // field only renders inside a navigation container.
    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                FilesInspectorTabPicker()
                    .padding(.horizontal, 12)
                    .padding(.bottom, 10)
                Divider()
                FilesContent()
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .navigationTitle("Files")
            .navigationBarTitleDisplayMode(.inline)
        }
    }
}

private struct FilesContent: View {
    @Environment(AppModel.self) private var model

    @ViewBuilder
    var body: some View {
        switch model.filesInspectorTab {
        case .unstaged: WorkspaceDiffView()
        case .allFiles: WorkspaceFileList()
        case .chatFiles: ChatFileList()
        }
    }
}

private struct FilesInspectorTabPicker: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Picker(
            "File collection",
            selection: Binding(
                get: { model.filesInspectorTab },
                set: { tab in model.selectFilesInspectorTab(tab) }
            )
        ) {
            ForEach(FilesInspectorTab.allCases) { tab in
                Text(tab.title).tag(tab)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .accessibilityLabel("File collection")
    }
}

private extension FilesInspectorTab {
    var title: String {
        switch self {
        case .unstaged: "Unstaged"
        case .allFiles: "All Files"
        case .chatFiles: "Chat Files"
        }
    }
}

extension String {
    var fileGlyph: HorusGlyph {
        switch URL(fileURLWithPath: self).pathExtension.lowercased() {
        case "py", "pyi", "pyw": .python
        case "ts", "tsx": .typeScript
        case "js", "jsx", "mjs", "cjs": .javaScript
        case "csv", "tsv": .csv
        case "rs": .rust
        case "go": .go
        case "md", "mdx", "markdown": .markdown
        case "swift", "c", "h", "cpp", "hpp", "java", "kt", "kts", "rb", "php", "sh", "zsh": .fileScript
        case "doc", "docx", "odt", "pages", "rtf": .doc
        case "png", "jpg", "jpeg", "gif", "heic", "webp", "svg": .image01
        case "json", "yaml", "yml", "toml", "xml", "ini", "plist": .gear
        default: .fileText
        }
    }

    var sourceHighlightLanguage: HighlightLanguage? {
        switch URL(fileURLWithPath: self).pathExtension.lowercased() {
        case "py", "pyi", "pyw": .python
        case "rs": .rust
        case "go": .go
        case "ts", "tsx": .typeScript
        case "js", "jsx", "mjs", "cjs": .javaScript
        case "md", "mdx", "markdown": .markdown
        case "swift": .swift
        case "c", "h": .c
        case "cpp", "hpp", "cc", "cxx": .cPlusPlus
        case "java": .java
        case "kt", "kts": .kotlin
        case "rb": .ruby
        case "php": .php
        case "sh", "zsh", "bash": .shell
        case "json": .json
        case "yaml", "yml": .yaml
        case "toml": .toml
        default: nil
        }
    }
}

private struct WorkspaceFileList: View {
    @Environment(AppModel.self) private var model
    @State private var tree: [FileTreeNode] = []
    @State private var query = ""
    @State private var matches: [WorkspaceFileRecord] = []
    @State private var matchedQuery = ""

    var body: some View {
        content
            .searchable(text: $query, placement: .toolbar, prompt: "Search files")
            .task(id: model.workspaceFilesRevision) {
                let files = model.workspaceFiles
                async let builtTree = FileTreeNode.tree(from: files)
                let result = await builtTree
                guard !Task.isCancelled else { return }
                tree = result
            }
            .task(id: searchRequest) {
                guard !query.isEmpty else {
                    matches = []
                    matchedQuery = ""
                    return
                }
                try? await Task.sleep(for: .milliseconds(120))
                guard !Task.isCancelled else { return }
                let files = model.workspaceFiles
                let query = query
                let searchTask = Task.detached(priority: .userInitiated) {
                    files.filter { $0.path.localizedCaseInsensitiveContains(query) }
                }
                let result = await searchTask.value
                guard !Task.isCancelled else { return }
                matches = result
                matchedQuery = query
            }
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoadingWorkspaceFiles {
            InspectorLoadingView(title: "Loading workspace files")
        } else if model.workspaceFiles.isEmpty {
            HorusUnavailable(title: "No workspace files", glyph: .fileMagnifyingGlass)
        } else if !query.isEmpty {
            searchResults
        } else {
            List {
                OutlineGroup(tree, children: \.children) { node in
                    if node.isFolder {
                        FileTreeRow(node: node)
                    } else {
                        fileButton(path: node.id, label: FileTreeRow(node: node))
                    }
                }
                .buttonStyle(.horusPlain)
                .inspectorFileListRow()
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
        }
    }

    /// A tree hides matches inside collapsed folders, so searching switches to the flat list
    /// of hits with their full path.
    @ViewBuilder
    private var searchResults: some View {
        if matchedQuery != query {
            InspectorLoadingView(title: "Searching files")
        } else if matches.isEmpty {
            HorusUnavailable(title: "No matching files", glyph: .magnifyingGlass)
        } else {
            List(matches) { file in
                fileButton(
                    path: file.path,
                    label: InspectorFileRow(
                        name: URL(fileURLWithPath: file.path).lastPathComponent,
                        detail: file.path,
                        size: Int64(clamping: file.size)
                    )
                )
                .inspectorFileListRow()
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
        }
    }

    private var searchRequest: WorkspaceFileSearchRequest {
        WorkspaceFileSearchRequest(query: query, catalogRevision: model.workspaceFilesRevision)
    }

    private func fileButton(path: String, label: some View) -> some View {
        Button {
            guard let file = model.workspaceFiles.first(where: { $0.path == path }) else { return }
            model.previewWorkspaceFile(file)
        } label: {
            label
        }
        .buttonStyle(.horusPlain)
        .disabled(model.isLoadingFilePresentation)
        .accessibilityLabel("Open workspace file \(path)")
    }
}

private struct WorkspaceFileSearchRequest: Equatable {
    let query: String
    let catalogRevision: Int
}

private struct FileTreeRow: View {
    @Environment(\.horusPalette) private var palette
    let node: FileTreeNode

    var body: some View {
        HStack(spacing: 10) {
            HorusIcon(
                node.isFolder ? .folder : node.id.fileGlyph,
                size: 15,
                foreground: node.isFolder ? palette.muted : palette.accent
            )
            Text(node.name)
                .font(HorusStyle.bodyFont)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 8)
            if let size = node.size {
                Text(size, format: .byteCount(style: .file))
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
            }
        }
        .frame(minHeight: 34)
        .contentShape(Rectangle())
    }
}

private struct ChatFileList: View {
    @Environment(AppModel.self) private var model

    private var artifactFiles: [SessionFileReference] {
        model.artifacts.compactMap(\.file)
    }

    var body: some View {
        List {
            Section("Artifacts") {
                if model.isLoadingArtifacts {
                    InspectorSectionLoadingRow(title: "Loading artifacts")
                } else if artifactFiles.isEmpty {
                    InspectorEmptyRow(
                        title: "No artifacts",
                        glyph: .fileAxisThreeD
                    )
                } else {
                    ForEach(artifactFiles) { file in
                        SessionFileInspectorRow(
                            file: file,
                            accessibilityLabel: "Open artifact \(file.name)"
                        )
                    }
                }
                if model.artifactsTruncated {
                    Text("Some older artifacts are not shown.")
                        .font(HorusStyle.metadataFont)
                        .foregroundStyle(.secondary)
                        .inspectorFileListRow()
                }
            }

            Section("Uploads") {
                if model.isLoadingSessionUploads {
                    InspectorSectionLoadingRow(title: "Loading uploads")
                } else if model.sessionUploads.isEmpty {
                    InspectorEmptyRow(
                        title: "No uploads",
                        glyph: .fileUpload
                    )
                } else {
                    ForEach(model.sessionUploads) { file in
                        SessionFileInspectorRow(
                            file: file,
                            accessibilityLabel: "Open uploaded file \(file.name)"
                        )
                    }
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
    }
}

private struct SessionFileInspectorRow: View {
    @Environment(AppModel.self) private var model
    let file: SessionFileReference
    let accessibilityLabel: String

    var body: some View {
        HStack(spacing: 0) {
            Button {
                model.previewSessionFile(file)
            } label: {
                InspectorFileRow(
                    name: file.name,
                    detail: file.mediaType,
                    size: file.size,
                    showsDisclosure: false
                )
            }
            .accessibilityLabel(accessibilityLabel)

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
            .accessibilityLabel("File actions for \(file.name)")
            .help("File actions")
        }
        .buttonStyle(.horusPlain)
        .disabled(model.isLoadingFilePresentation)
        .inspectorFileListRow()
    }
}

private struct InspectorSectionLoadingRow: View {
    let title: String

    var body: some View {
        HStack(spacing: 10) {
            ProgressView().controlSize(.small)
            Text(title).foregroundStyle(.secondary)
        }
        .frame(minHeight: HorusStyle.iconButtonSize)
        .accessibilityElement(children: .combine)
    }
}

private struct InspectorEmptyRow: View {
    @Environment(\.horusPalette) private var palette
    let title: String
    let glyph: HorusGlyph

    var body: some View {
        VStack(spacing: 8) {
            HorusIcon(glyph, size: 44, foreground: palette.muted)
            Text(title)
                .font(HorusStyle.metadataFont.weight(.semibold))
                .foregroundStyle(palette.muted)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 16)
        .listRowBackground(Color.clear)
        .listRowSeparator(.hidden)
        .accessibilityElement(children: .combine)
    }
}

private struct InspectorFileListRow: ViewModifier {
    @Environment(\.horusPalette) private var palette

    func body(content: Content) -> some View {
        content
            .listRowInsets(EdgeInsets(top: 4, leading: 16, bottom: 4, trailing: 12))
            .listRowBackground(Color.clear)
            .listRowSeparatorTint(palette.line)
    }
}

extension View {
    fileprivate func inspectorFileListRow() -> some View {
        modifier(InspectorFileListRow())
    }
}

private struct InspectorFileRow: View {
    @Environment(\.horusPalette) private var palette
    let name: String
    let detail: String
    let size: Int64
    var showsDisclosure = true

    var body: some View {
        HStack(spacing: 10) {
            HorusIcon(name.fileGlyph, foreground: palette.accent)
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                    .font(HorusStyle.metadataFont.weight(.semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(detail)
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer(minLength: 8)
            Text(size, format: .byteCount(style: .file))
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
            if showsDisclosure {
                HorusIcon(.caretRight, size: 12, foreground: palette.muted)
            }
        }
        .frame(minHeight: HorusStyle.iconButtonSize)
        .contentShape(Rectangle())
    }
}

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
    @Environment(\.horusPalette) private var palette
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
                            .font(HorusStyle.metadataFont)
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                            .frame(width: 44, alignment: .trailing)
                            .padding(.trailing, 12)
                        Text(line.text.characters.isEmpty ? AttributedString(" ") : line.text)
                            .font(HorusStyle.metadataFont)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 16)
            .padding(.trailing, 16)
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
    @Environment(\.horusPalette) private var palette
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
                            if let next = currentPreview.next {
                                TranscriptPaginationButton(
                                    isLoading: model.isLoadingPreviewPage,
                                    isEnabled: !model.isLoadingPreviewPage
                                ) {
                                    retainedEntryID = currentPreview.entries.first?.id
                                    model.loadPreviewPage(next)
                                }
                                .padding(.bottom, 12)
                            }
                            TranscriptRowsView(
                                entries: currentPreview.entries,
                                activeStepID: nil,
                                breakBefore: retainedEntryID,
                                collapsesLongMessages: true
                            )
                        }
                        .scrollTargetLayout()
                        .frame(maxWidth: 880)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(16)
                    }
                    .scrollIndicators(.hidden)
                    .refreshable { loadEarlierPage() }
                    .onChange(of: currentPreview.entries.count) { _, _ in
                        guard let retainedEntryID else { return }
                        proxy.scrollTo(retainedEntryID, anchor: .top)
                        self.retainedEntryID = nil
                    }
                }

                if model.isLoadingPreviewPage {
                    ZStack {
                        palette.canvas.opacity(0.58)
                        HorusComposingOrb()
                            .frame(width: 112, height: 112)
                    }
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("Loading earlier agent messages")
                }
            }
        }
        .background(HorusBackdrop())
        .presentationDetents([.medium, .large], selection: $selectedDetent)
    }

    private var header: some View {
        HStack(spacing: 6) {
            Text(agentName)
                .font(HorusStyle.controlFont.weight(.semibold))
                .lineLimit(1)
                .truncationMode(.middle)
            if let status = currentPreview.status {
                headerSeparator
                Text(status)
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(status == "errored" ? palette.danger : palette.muted)
                    .lineLimit(1)
            }
            if let choice = modelChoice {
                headerSeparator
                HorusMenuLabel(
                    text: model.modelLabel(for: choice),
                    glyph: model.providerSymbol(for: choice)
                        .flatMap(HorusSymbol.knownGlyph(for:)) ?? .robot,
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
        .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize, alignment: .leading)
        .padding(.leading, 16)
        .padding(.trailing, HorusStyle.iconRowPadding)
        .padding(.vertical, 6)
        .accessibilityElement(children: .contain)
    }

    private var headerSeparator: some View {
        Text("•")
            .font(HorusStyle.metadataFont)
            .foregroundStyle(palette.muted)
            .accessibilityHidden(true)
    }

    private func loadEarlierPage() {
        guard let next = currentPreview.next, !model.isLoadingPreviewPage else { return }
        retainedEntryID = currentPreview.entries.first?.id
        model.loadPreviewPage(next)
    }

    private var currentPreview: TranscriptPreview {
        if model.presentedPreview?.id == preview.id, let presented = model.presentedPreview {
            return presented
        }
        return model.previews.first(where: { $0.id == preview.id }) ?? preview
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

    private var spawnContextGlyph: HorusGlyph {
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
    @Environment(\.horusPalette) private var palette
    let block: FrontendBlock

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(block.files) { file in
                SessionFileCard(file: file)
            }
            if !block.text.isEmpty {
                HStack(alignment: .top, spacing: 8) {
                    if block.pending { ProgressView().controlSize(.mini) }
                    CollapsibleText(text: block.text)
                        .font(
                            block.format == "unified_diff"
                                ? HorusStyle.metadataFont
                                : HorusStyle.bodyFont
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
