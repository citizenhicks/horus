import Foundation
import SwiftUI
import HighlightSwift

struct FilesView: View {
    @Environment(AppModel.self) private var model

    // A NavigationStack for the title and, more to the point, for `.searchable`: the search
    // field only renders inside a navigation container.
    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                FilesInspectorTabPicker()
                    .padding(.horizontal, HorusSpace.m)
                    .padding(.bottom, HorusSpace.m)
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
        case .unstaged:
            WorkspaceDiffView(
                source: model.gitDiff,
                revision: model.gitDiffRevision,
                isLoading: model.isLoadingGitDiff,
                title: "unstaged changes"
            )
            .id(FilesInspectorTab.unstaged)
        case .committed:
            WorkspaceDiffView(
                source: model.committedGitDiff,
                revision: model.committedGitDiffRevision,
                isLoading: model.isLoadingCommittedGitDiff,
                title: "last commit"
            )
            .id(FilesInspectorTab.committed)
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
        case .committed: "Committed"
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
    @Environment(\.horusPalette) private var palette
    @State private var tree: [FileTreeNode] = []
    @State private var query = ""
    @State private var matches: [WorkspaceFileRecord] = []
    @State private var matchedQuery = ""

    var body: some View {
        VStack(spacing: 0) {
            if model.workspaceFilesTruncated && !model.isLoadingWorkspaceFiles {
                HStack(spacing: HorusSpace.s) {
                    HorusIcon(
                        .warning,
                        size: HorusStyle.glyphInline,
                        foreground: palette.warning
                    )
                    Text("Some workspace files are not shown. Ignore generated folders to keep the catalog focused.")
                        .font(HorusStyle.metadataFont)
                        .foregroundStyle(palette.muted)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, HorusSpace.m)
                .padding(.vertical, HorusSpace.s)
                .accessibilityElement(children: .combine)
                Divider()
            }
            content
        }
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
        HStack(spacing: HorusSpace.m) {
            HorusIcon(
                node.isFolder ? .folder : node.id.fileGlyph,
                size: 15,
                foreground: node.isFolder ? palette.muted : palette.accent
            )
            Text(node.name)
                .font(HorusStyle.bodyFont)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: HorusSpace.s)
            if let size = node.size {
                Text(size, format: .byteCount(style: .file))
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
            }
        }
        .frame(minHeight: HorusStyle.rowRegular)
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
                HorusIcon(.dotsThree, size: HorusStyle.glyphInline)
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
        HStack(spacing: HorusSpace.m) {
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
        VStack(spacing: HorusSpace.s) {
            HorusIcon(glyph, size: 44, foreground: palette.muted)
            Text(title)
                .font(HorusStyle.metadataFont.weight(.semibold))
                .foregroundStyle(palette.muted)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, HorusSpace.l)
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
        HStack(spacing: HorusSpace.m) {
            HorusIcon(name.fileGlyph, foreground: palette.accent)
            VStack(alignment: .leading, spacing: HorusSpace.xxs) {
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
            Spacer(minLength: HorusSpace.s)
            Text(size, format: .byteCount(style: .file))
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
            if showsDisclosure {
                HorusIcon(.caretRight, size: HorusStyle.glyphMark, foreground: palette.muted)
            }
        }
        .frame(minHeight: HorusStyle.iconButtonSize)
        .contentShape(Rectangle())
    }
}
