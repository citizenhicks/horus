import Foundation
import SwiftUI
import HighlightSwift

struct FilesView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette

    // A NavigationStack for the title and, more to the point, for `.searchable`: the search
    // field only renders inside a navigation container.
    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                FilesInspectorTabPicker()
                    .padding(.horizontal, MobiusSpace.m)
                    .padding(.bottom, MobiusSpace.m)
                Divider()
                FilesContent()
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            // The same step under the canvas the sidebar sits on: the inspector is the other
            // column flanking the page, so it reads as a pair with it rather than as more page.
            .background { palette.recessed.ignoresSafeArea() }
            .navigationTitle(navigationTitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    FilesNavigationTitle()
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { model.showsInspector = false }
                }
            }
        }
    }

    private var navigationTitle: String {
        if model.filesInspectorTab == .modified { return model.modifiedFilesScope.title }
        return model.filesInspectorTab.title
    }
}

private struct FilesContent: View {
    @Environment(AppModel.self) private var model

    @ViewBuilder
    var body: some View {
        switch model.filesInspectorTab {
        case .modified: ModifiedFilesDiff()
        case .allFiles: WorkspaceFileList()
        case .chatFiles: ChatFileList()
        }
    }
}

private struct FilesNavigationTitle: View {
    @Environment(AppModel.self) private var model

    @ViewBuilder
    var body: some View {
        if model.filesInspectorTab == .modified {
            ModifiedFilesScopePicker()
        } else {
            Text(model.filesInspectorTab.title)
                .font(MobiusStyle.titleFont)
        }
    }
}

private struct ModifiedFilesDiff: View {
    @Environment(AppModel.self) private var model

    @ViewBuilder
    var body: some View {
        switch model.modifiedFilesScope {
        case .unstaged:
            WorkspaceDiffView(
                source: model.gitDiff,
                revision: model.gitDiffRevision,
                isLoading: model.isLoadingGitDiff,
                title: "unstaged changes"
            )
            .id(GitDiffScope.unstaged)
        case .staged:
            WorkspaceDiffView(
                source: model.stagedGitDiff,
                revision: model.stagedGitDiffRevision,
                isLoading: model.isLoadingStagedGitDiff,
                title: "staged changes"
            )
            .id(GitDiffScope.staged)
        case .committed:
            WorkspaceDiffView(
                source: model.committedGitDiff,
                revision: model.committedGitDiffRevision,
                isLoading: model.isLoadingCommittedGitDiff,
                title: "last commit"
            )
            .id(GitDiffScope.committed)
        }
    }
}

private struct ModifiedFilesScopePicker: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Menu {
            ForEach(GitDiffScope.allCases) { scope in
                Button {
                    model.selectModifiedFilesScope(scope)
                } label: {
                    MobiusLabel(
                        title: scope.title,
                        glyph: scope == model.modifiedFilesScope ? .check : .gitBranch
                    )
                }
            }
        } label: {
            HStack(spacing: MobiusSpace.xs) {
                Text(model.modifiedFilesScope.title)
                    .font(MobiusStyle.titleFont)
                MobiusIcon(.caretDown, size: MobiusStyle.glyphMark, foreground: .secondary)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.mobiusPlain)
        .menuIndicator(.hidden)
        .tint(.primary)
        .accessibilityLabel("Modified file view")
        .accessibilityValue(model.modifiedFilesScope.title)
        .help("Choose which Git changes to show")
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
        case .modified: "Modified"
        case .allFiles: "All Files"
        case .chatFiles: "Chat Files"
        }
    }
}

private extension GitDiffScope {
    var title: String {
        switch self {
        case .unstaged: "Unstaged"
        case .staged: "Staged"
        case .committed: "Last Commit"
        }
    }
}

extension String {
    var fileGlyph: MobiusGlyph {
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
    @Environment(\.mobiusPalette) private var palette
    @State private var tree: [FileTreeNode] = []
    @State private var query = ""
    @State private var matches: [WorkspaceFileRecord] = []
    @State private var matchedQuery = ""

    var body: some View {
        VStack(spacing: 0) {
            if model.workspaceFilesTruncated && !model.isLoadingWorkspaceFiles {
                HStack(spacing: MobiusSpace.s) {
                    MobiusIcon(
                        .warning,
                        size: MobiusStyle.glyphInline,
                        foreground: palette.warning
                    )
                    Text("Some workspace files are not shown. Ignore generated folders to keep the catalog focused.")
                        .font(MobiusStyle.metadataFont)
                        .foregroundStyle(palette.muted)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, MobiusSpace.m)
                .padding(.vertical, MobiusSpace.s)
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
            MobiusUnavailable(title: "No workspace files", glyph: .fileMagnifyingGlass)
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
                .buttonStyle(.mobiusPlain)
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
            MobiusUnavailable(title: "No matching files", glyph: .magnifyingGlass)
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
        .buttonStyle(.mobiusPlain)
        .disabled(model.isLoadingFilePresentation)
        .accessibilityLabel("Open workspace file \(path)")
    }
}

private struct WorkspaceFileSearchRequest: Equatable {
    let query: String
    let catalogRevision: Int
}

private struct FileTreeRow: View {
    @Environment(\.mobiusPalette) private var palette
    let node: FileTreeNode

    var body: some View {
        HStack(spacing: MobiusSpace.m) {
            MobiusIcon(
                node.isFolder ? .folder : node.id.fileGlyph,
                size: 15,
                foreground: node.isFolder ? palette.muted : palette.accent
            )
            Text(node.name)
                .font(MobiusStyle.bodyFont)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: MobiusSpace.s)
            if let size = node.size {
                Text(size, format: .byteCount(style: .file))
                    .font(MobiusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
            }
        }
        .frame(minHeight: MobiusStyle.rowRegular)
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
                        .font(MobiusStyle.metadataFont)
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
                MobiusIcon(.dotsThree, size: MobiusStyle.glyphInline)
                    .frame(width: MobiusStyle.iconButtonSize, height: MobiusStyle.iconButtonSize)
                    .contentShape(Rectangle())
            }
            .accessibilityLabel("File actions for \(file.name)")
            .help("File actions")
        }
        .buttonStyle(.mobiusPlain)
        .disabled(model.isLoadingFilePresentation)
        .inspectorFileListRow()
    }
}

private struct InspectorSectionLoadingRow: View {
    let title: String

    var body: some View {
        HStack(spacing: MobiusSpace.m) {
            ProgressView().controlSize(.small)
            Text(title).foregroundStyle(.secondary)
        }
        .frame(minHeight: MobiusStyle.iconButtonSize)
        .accessibilityElement(children: .combine)
    }
}

private struct InspectorEmptyRow: View {
    @Environment(\.mobiusPalette) private var palette
    let title: String
    let glyph: MobiusGlyph

    var body: some View {
        VStack(spacing: MobiusSpace.s) {
            MobiusIcon(glyph, size: 44, foreground: palette.muted)
            Text(title)
                .font(MobiusStyle.metadataFont.weight(.semibold))
                .foregroundStyle(palette.muted)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, MobiusSpace.l)
        .listRowBackground(Color.clear)
        .listRowSeparator(.hidden)
        .accessibilityElement(children: .combine)
    }
}

private struct InspectorFileListRow: ViewModifier {
    @Environment(\.mobiusPalette) private var palette

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
    @Environment(\.mobiusPalette) private var palette
    let name: String
    let detail: String
    let size: Int64
    var showsDisclosure = true

    var body: some View {
        HStack(spacing: MobiusSpace.m) {
            MobiusIcon(name.fileGlyph, foreground: palette.accent)
            VStack(alignment: .leading, spacing: MobiusSpace.xxs) {
                Text(name)
                    .font(MobiusStyle.metadataFont.weight(.semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(detail)
                    .font(MobiusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer(minLength: MobiusSpace.s)
            Text(size, format: .byteCount(style: .file))
                .font(MobiusStyle.metadataFont)
                .foregroundStyle(palette.muted)
            if showsDisclosure {
                MobiusIcon(.caretRight, size: MobiusStyle.glyphMark, foreground: palette.muted)
            }
        }
        .frame(minHeight: MobiusStyle.iconButtonSize)
        .contentShape(Rectangle())
    }
}
