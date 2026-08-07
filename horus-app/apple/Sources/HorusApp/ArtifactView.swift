import SwiftUI
import HighlightSwift

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
            WorkspaceFileList()
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
                get: { model.workspaceFileScope },
                set: { scope in model.selectWorkspaceFileScope(scope) }
            )
        ) {
            ForEach(WorkspaceFileScope.allCases) { scope in
                Text(scope.title).tag(scope)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .accessibilityLabel("File scope")
    }
}

private extension WorkspaceFileScope {
    var title: String {
        switch self {
        case .modified: "Modified"
        case .all: "All Files"
        }
    }

    var emptyTitle: String {
        switch self {
        case .modified: "No modified files"
        case .all: "No workspace files"
        }
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
            HorusUnavailable(title: model.workspaceFileScope.emptyTitle, glyph: .fileMagnifyingGlass)
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

struct TextFilePreviewView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.horusPalette) private var palette
    let preview: TextFilePreview

    var body: some View {
        NavigationStack {
            ScrollView([.horizontal, .vertical]) {
                CodeText(preview.contents)
                    .font(HorusStyle.metadataFont)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
            }
            .background(palette.canvas)
            .navigationTitle(preview.name)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done", action: dismiss.callAsFunction)
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 640, minHeight: 560)
        #endif
        .presentationDetents([.large])
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
