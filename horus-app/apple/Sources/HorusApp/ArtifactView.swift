import SwiftUI

struct ArtifactView: View {
    @Environment(AppModel.self) private var model

    // A NavigationStack for the title and, more to the point, for `.searchable`: the search
    // field only renders inside a navigation container.
    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if model.inspectorPage == .changes {
                    WorkspaceScopePicker()
                        .padding(.horizontal, 12)
                        .padding(.bottom, 10)
                    Divider()
                }
                ArtifactContent()
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .navigationTitle(model.inspectorPage == .changes ? "Files" : "Uploaded files")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
        }
    }
}

private struct ArtifactContent: View {
    @Environment(AppModel.self) private var model

    @ViewBuilder
    var body: some View {
        switch model.inspectorPage {
        case .changes:
            if model.workspaceViewerScope == .all {
                WorkspaceFileList()
            } else if model.isLoadingGitDiff {
                InspectorLoadingView(title: "Loading changes")
            } else if model.gitDiff.isEmpty {
                HorusUnavailable(
                    title: "No \(model.workspaceViewerScope.title.lowercased()) changes",
                    glyph: .fileMagnifyingGlass
                )
            } else {
                UnifiedDiffView(document: UnifiedDiffDocument(model.gitDiff))
            }
        case .uploads:
            UploadedFileList()
        }
    }
}

private struct WorkspaceScopePicker: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Picker(
            "File scope",
            selection: Binding(
                get: { model.workspaceViewerScope },
                set: { scope in model.selectWorkspaceViewerScope(scope) }
            )
        ) {
            ForEach(WorkspaceViewerScope.allCases) { scope in
                Text(scope.title).tag(scope)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .accessibilityLabel("File scope")
    }
}

private struct WorkspaceFileList: View {
    @Environment(AppModel.self) private var model
    @State private var tree: [FileTreeNode] = []
    @State private var query = ""

    var body: some View {
        content
            .searchable(text: $query, placement: .toolbar, prompt: "Search files")
            .task(id: model.workspaceFiles) { tree = FileTreeNode.tree(from: model.workspaceFiles) }
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
        let matches = model.workspaceFiles.filter {
            $0.path.localizedCaseInsensitiveContains(query)
        }
        if matches.isEmpty {
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

    private func fileButton(path: String, label: some View) -> some View {
        Button {
            guard let file = model.workspaceFiles.first(where: { $0.path == path }) else { return }
            model.previewWorkspaceFile(file)
        } label: {
            label
        }
        .buttonStyle(.horusPlain)
        .disabled(model.isLoadingAttachmentPreview)
        .accessibilityLabel("Open workspace file \(path)")
    }
}

private struct FileTreeRow: View {
    @Environment(\.horusPalette) private var palette
    let node: FileTreeNode

    var body: some View {
        HStack(spacing: 10) {
            HorusIcon(
                node.isFolder ? .folder : .fileText,
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

private struct UploadedFileList: View {
    @Environment(AppModel.self) private var model

    @ViewBuilder
    var body: some View {
        if model.isLoadingAttachments {
            InspectorLoadingView(title: "Loading uploaded files")
        } else if model.uploadedAttachments.isEmpty {
            HorusUnavailable(
                title: "No uploaded files",
                glyph: .fileText,
                detail: "Use the Plus button in the composer to add files."
            )
        } else {
            List(model.uploadedAttachments) { attachment in
                Button {
                    model.previewAttachment(attachment)
                } label: {
                    InspectorFileRow(
                        name: attachment.name,
                        detail: attachment.mediaType,
                        size: attachment.size
                    )
                }
                .buttonStyle(.horusPlain)
                .disabled(model.isLoadingAttachmentPreview)
                .accessibilityLabel("Open uploaded file \(attachment.name)")
                .inspectorFileListRow()
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
        }
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

    var body: some View {
        HStack(spacing: 10) {
            HorusIcon(.fileText, foreground: palette.accent)
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
            HorusIcon(.caretRight, size: 12, foreground: palette.muted)
        }
        .frame(minHeight: HorusStyle.iconButtonSize)
        .contentShape(Rectangle())
    }
}

private struct InspectorLoadingView: View {
    let title: String

    var body: some View {
        ProgressView(title)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityLabel(title)
    }
}

struct UnifiedDiffDocument: Equatable {
    let files: [DiffFile]

    init(_ source: String) {
        var files: [DiffFile] = []
        var currentPath = "Code changes"
        var hunks: [DiffHunk] = []
        var header = ""
        var lines: [DiffLine] = []
        var oldLine: Int?
        var newLine: Int?
        var nextHunkID = 0
        var nextLineID = 0
        var nextFileID = 0

        func appendLine(_ kind: DiffLine.Kind, old: Int?, new: Int?, text: String) {
            lines.append(DiffLine(id: nextLineID, kind: kind, oldNumber: old, newNumber: new, text: text))
            nextLineID += 1
        }

        func flushHunk() {
            guard !header.isEmpty else {
                lines = []
                return
            }
            hunks.append(DiffHunk(
                id: nextHunkID,
                header: header,
                lines: lines
            ))
            nextHunkID += 1
            header = ""
            lines = []
            oldLine = nil
            newLine = nil
        }

        func flushFile() {
            flushHunk()
            guard !hunks.isEmpty else { return }
            files.append(DiffFile(id: nextFileID, path: currentPath, hunks: hunks))
            nextFileID += 1
            hunks = []
        }

        for rawSlice in source.split(separator: "\n", omittingEmptySubsequences: false) {
            let raw = String(rawSlice)
            if raw.hasPrefix("diff --git ") {
                flushFile()
                currentPath = Self.path(fromGitHeader: raw) ?? currentPath
            } else if raw.hasPrefix("+++ ") {
                currentPath = Self.cleanPath(String(raw.dropFirst(4))) ?? currentPath
            } else if raw.hasPrefix("--- ") {
                continue
            } else if raw.hasPrefix("@@") {
                flushHunk()
                header = raw
                let starts = Self.hunkStarts(raw)
                oldLine = starts.old
                newLine = starts.new
            } else if raw.hasPrefix("+") {
                appendLine(.addition, old: nil, new: newLine, text: String(raw.dropFirst()))
                newLine = newLine.map { $0 + 1 }
            } else if raw.hasPrefix("-") {
                appendLine(.removal, old: oldLine, new: nil, text: String(raw.dropFirst()))
                oldLine = oldLine.map { $0 + 1 }
            } else if raw.hasPrefix(" ") {
                appendLine(.context, old: oldLine, new: newLine, text: String(raw.dropFirst()))
                oldLine = oldLine.map { $0 + 1 }
                newLine = newLine.map { $0 + 1 }
            } else if !raw.isEmpty && !header.isEmpty {
                appendLine(.metadata, old: nil, new: nil, text: raw)
            }
        }
        flushFile()
        self.files = files
    }

    var added: Int { files.reduce(0) { $0 + $1.added } }
    var removed: Int { files.reduce(0) { $0 + $1.removed } }

    private static func path(fromGitHeader header: String) -> String? {
        let parts = header.split(separator: " ")
        guard parts.count >= 4 else { return nil }
        return cleanPath(String(parts[3]))
    }

    private static func cleanPath(_ raw: String) -> String? {
        guard raw != "/dev/null" else { return nil }
        return raw.hasPrefix("b/") || raw.hasPrefix("a/") ? String(raw.dropFirst(2)) : raw
    }

    private static func hunkStarts(_ header: String) -> (old: Int?, new: Int?) {
        let fields = header.split(separator: " ")
        func start(_ marker: Character) -> Int? {
            guard let field = fields.first(where: { $0.first == marker }) else { return nil }
            return Int(field.dropFirst().split(separator: ",", maxSplits: 1)[0])
        }
        return (start("-"), start("+"))
    }
}

struct DiffFile: Identifiable, Equatable {
    let id: Int
    let path: String
    let hunks: [DiffHunk]
    var added: Int { hunks.reduce(0) { $0 + $1.lines.filter { $0.kind == .addition }.count } }
    var removed: Int { hunks.reduce(0) { $0 + $1.lines.filter { $0.kind == .removal }.count } }
}

struct DiffHunk: Identifiable, Equatable {
    let id: Int
    let header: String
    let lines: [DiffLine]
}

struct DiffLine: Identifiable, Equatable {
    enum Kind: Equatable { case addition, removal, context, metadata }
    let id: Int
    let kind: Kind
    let oldNumber: Int?
    let newNumber: Int?
    let text: String

}

private struct UnifiedDiffView: View {
    let document: UnifiedDiffDocument

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                ForEach(document.files) { file in
                    DiffFileView(file: file)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14)
        }
        .scrollIndicators(.hidden)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Code diff, \(document.files.count) files, \(document.added) additions and \(document.removed) removals")
    }
}

private struct DiffFileView: View {
    @Environment(\.horusPalette) private var palette
    @State private var isExpanded = false
    let file: DiffFile

    var body: some View {
        DisclosureGroup(isExpanded: $isExpanded) {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(file.hunks) { hunk in
                    DiffHunkView(hunk: hunk)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            HStack(spacing: 8) {
                HorusIcon(.fileText, foreground: palette.accent)
                Text(file.path)
                    .font(HorusStyle.metadataFont.weight(.semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 6)
                Text("+\(file.added)").foregroundStyle(palette.signal)
                Text("−\(file.removed)").foregroundStyle(palette.danger)
            }
            .font(HorusStyle.metadataFont.weight(.semibold))
            .padding(.vertical, 10)
            .frame(minHeight: HorusStyle.iconButtonSize)
        }
        .padding(.horizontal, 12)
        .horusGlass(in: HorusStyle.controlShape, interactive: true)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("File \(file.path), \(file.added) additions, \(file.removed) removals")
    }
}

private struct DiffHunkView: View {
    @Environment(\.horusPalette) private var palette
    let hunk: DiffHunk

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 0) {
            Text(hunk.header)
                .font(HorusStyle.metadataFont.weight(.bold))
                .foregroundStyle(palette.accent)
                .padding(.horizontal, 11)
                .padding(.vertical, 7)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(palette.accentSoft.opacity(0.45))
                .accessibilityLabel("Hunk \(hunk.header)")
            ForEach(hunk.lines) { line in
                DiffLineView(line: line)
            }
        }
    }
}

private struct DiffLineView: View {
    @Environment(\.horusPalette) private var palette
    let line: DiffLine

    var body: some View {
        HStack(spacing: 0) {
            gutter(line.oldNumber)
            gutter(line.newNumber)
            Text(marker)
                .font(HorusStyle.metadataFont.weight(.bold))
                .foregroundStyle(markerColor)
                .frame(width: 24)
            Text(line.text.isEmpty ? " " : line.text)
                .font(HorusStyle.metadataFont)
                .textSelection(.enabled)
                .padding(.trailing, 12)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, minHeight: 23, alignment: .leading)
        .background(background)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    private func gutter(_ number: Int?) -> some View {
        Text(number.map(String.init) ?? "")
            .font(HorusStyle.metadataFont)
            .foregroundStyle(palette.muted)
            .frame(width: 30, alignment: .trailing)
            .padding(.trailing, 5)
            .frame(maxHeight: .infinity)
            .background(palette.canvas.opacity(0.42))
            .overlay(alignment: .trailing) { Rectangle().fill(palette.line.opacity(0.6)).frame(width: 0.5) }
    }

    private var marker: String {
        switch line.kind {
        case .addition: "+"
        case .removal: "−"
        case .context: " "
        case .metadata: "·"
        }
    }

    private var markerColor: Color {
        switch line.kind {
        case .addition: palette.signal
        case .removal: palette.danger
        case .context, .metadata: palette.muted
        }
    }

    private var background: Color {
        switch line.kind {
        case .addition: palette.signal.opacity(0.14)
        case .removal: palette.danger.opacity(0.14)
        case .context: palette.panel
        case .metadata: palette.raised.opacity(0.72)
        }
    }

    private var accessibilityLabel: String {
        let location: String
        switch (line.oldNumber, line.newNumber) {
        case let (old?, new?): location = "old line \(old), new line \(new)"
        case let (old?, nil): location = "old line \(old)"
        case let (nil, new?): location = "new line \(new)"
        default: location = "metadata"
        }
        let change: String
        switch line.kind {
        case .addition: change = "Added"
        case .removal: change = "Removed"
        case .context: change = "Context"
        case .metadata: change = "Metadata"
        }
        return "\(change), \(location): \(line.text)"
    }
}

struct PreviewTranscriptSheet: View {
    @Environment(\.dismiss) private var dismiss
    let preview: TranscriptPreview

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                VStack(spacing: 8) {
                    Text(preview.title)
                        .font(.title2)
                        .bold()
                    HStack(spacing: 8) {
                        if let status = preview.status {
                            HorusBadge(text: status, tone: status == "errored" ? "error" : "neutral")
                        }
                        if let model = preview.model {
                            Text(model)
                                .font(HorusStyle.metadataFont)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .frame(maxWidth: .infinity)
                .padding()

                Divider()

                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 10) {
                        ForEach(preview.blocks) { block in
                            PreviewBlockView(block: block.block)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
                }
                .scrollIndicators(.hidden)
            }
            #if os(macOS)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Close", glyph: .x, action: dismiss.callAsFunction)
                        .labelStyle(.iconOnly)
                }
            }
            #endif
        }
        #if os(macOS)
        .frame(minWidth: 560, minHeight: 520)
        #endif
        .presentationDetents([.medium, .large])
    }
}

struct PreviewBlockView: View {
    @Environment(\.horusPalette) private var palette
    let block: FrontendBlock

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            if block.pending { ProgressView().controlSize(.mini) }
            Text(block.text)
                .font(block.format == "unified_diff" ? HorusStyle.metadataFont : HorusStyle.bodyFont)
                .foregroundStyle(
                    block.tone == "neutral" ? Color.primary : palette.tone(block.tone)
                )
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
