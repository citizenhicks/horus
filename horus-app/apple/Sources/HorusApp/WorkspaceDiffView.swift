import SwiftUI

private struct RenderedWorkspaceDiff: Sendable {
    let revision: Int
    let document: UnifiedDiffDocument
}

struct WorkspaceDiffView: View {
    @Environment(AppModel.self) private var model
    @State private var rendered: RenderedWorkspaceDiff?
    @State private var expandedFileIDs: Set<Int> = []

    var body: some View {
        content
            .task(id: model.gitDiffRevision) {
                let revision = model.gitDiffRevision
                let source = model.gitDiff
                rendered = nil
                expandedFileIDs.removeAll()
                guard !source.isEmpty else { return }

                let parseTask = Task.detached(priority: .userInitiated) {
                    UnifiedDiffDocument(source)
                }
                let document = await withTaskCancellationHandler {
                    await parseTask.value
                } onCancel: {
                    parseTask.cancel()
                }
                guard !Task.isCancelled, model.gitDiffRevision == revision else { return }
                rendered = RenderedWorkspaceDiff(revision: revision, document: document)
            }
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoadingGitDiff {
            InspectorLoadingView(title: "Loading unstaged changes")
        } else if model.gitDiff.isEmpty {
            HorusUnavailable(title: "No unstaged changes", glyph: .gitBranch)
        } else if let rendered, rendered.revision == model.gitDiffRevision {
            if rendered.document.files.isEmpty {
                HorusUnavailable(title: "No displayable changes", glyph: .gitBranch)
            } else {
                UnifiedDiffView(
                    document: rendered.document,
                    expandedFileIDs: $expandedFileIDs
                )
            }
        } else {
            InspectorLoadingView(title: "Preparing unstaged changes")
        }
    }
}

private struct UnifiedDiffView: View {
    @Environment(\.horusPalette) private var palette
    let document: UnifiedDiffDocument
    @Binding var expandedFileIDs: Set<Int>

    var body: some View {
        List {
            ForEach(document.files) { file in
                DiffFileHeader(
                    file: file,
                    isExpanded: expandedFileIDs.contains(file.id),
                    toggle: { toggle(file.id) }
                )
                .diffListRow(topPadding: 10)

                if expandedFileIDs.contains(file.id) {
                    ForEach(file.rows) { row in
                        DiffRowView(row: row)
                            .diffListRow(bottomPadding: row.id == file.rows.last?.id ? 10 : 0)
                    }
                }
            }

            if document.isTruncated {
                HStack(spacing: 8) {
                    HorusIcon(.warning, size: 14, foreground: palette.warning)
                    Text("Diff truncated at the safe transfer limit")
                        .font(HorusStyle.metadataFont)
                        .foregroundStyle(palette.muted)
                }
                .diffListRow(topPadding: 8, bottomPadding: 12)
            }
        }
        .environment(\.defaultMinListRowHeight, 0)
        .listStyle(.plain)
        .listRowSpacing(0)
        .scrollContentBackground(.hidden)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(
            "Code diff, \(document.files.count) files, "
                + "\(document.added) additions and \(document.removed) removals"
        )
    }

    private func toggle(_ id: Int) {
        if expandedFileIDs.contains(id) {
            expandedFileIDs.remove(id)
        } else {
            expandedFileIDs.insert(id)
        }
    }
}

private struct DiffFileHeader: View {
    @Environment(\.horusPalette) private var palette
    let file: UnifiedDiffFile
    let isExpanded: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack(spacing: 10) {
                HorusIcon(file.path.fileGlyph, size: 17, foreground: palette.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text(file.name)
                        .font(HorusStyle.metadataFont.weight(.semibold))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let parentPath = file.parentPath {
                        Text(parentPath)
                            .font(HorusStyle.metadataFont)
                            .foregroundStyle(palette.muted)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                Spacer(minLength: 6)
                HStack(spacing: 5) {
                    Text("+\(file.added)").foregroundStyle(palette.signal)
                    Text("−\(file.removed)").foregroundStyle(palette.danger)
                }
                .font(HorusStyle.metadataFont.weight(.semibold))
                .fixedSize()
                HorusIcon(.caretRight, size: 11, foreground: palette.muted)
                    .rotationEffect(.degrees(isExpanded ? 90 : 0))
                    .animation(.snappy(duration: 0.18), value: isExpanded)
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 54)
            .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .background(palette.raised, in: headerShape)
        .overlay { headerShape.stroke(palette.line.opacity(0.55), lineWidth: 0.5) }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "File \(file.path), \(file.added) additions, \(file.removed) removals"
        )
        .accessibilityValue(isExpanded ? "Expanded" : "Collapsed")
        .accessibilityHint(isExpanded ? "Collapses this file" : "Shows changed lines")
    }

    private var headerShape: UnevenRoundedRectangle {
        let bottomRadius = isExpanded ? 0 : HorusStyle.tileRadius
        return UnevenRoundedRectangle(
            cornerRadii: .init(
                topLeading: HorusStyle.tileRadius,
                bottomLeading: bottomRadius,
                bottomTrailing: bottomRadius,
                topTrailing: HorusStyle.tileRadius
            ),
            style: .continuous
        )
    }
}

private struct DiffRowView: View {
    @Environment(\.horusPalette) private var palette
    let row: UnifiedDiffRow

    @ViewBuilder
    var body: some View {
        switch row.kind {
        case let .hunk(hunk):
            hunkHeader(hunk)
        case .addition, .removal, .context, .metadata:
            codeLine
        }
    }

    private func hunkHeader(_ hunk: UnifiedDiffHunk) -> some View {
        HStack(spacing: 8) {
            HorusIcon(.caretDown, size: 10, foreground: palette.muted)
            Text(hunk.title)
                .font(HorusStyle.metadataFont.weight(.semibold))
                .foregroundStyle(palette.muted)
            Spacer(minLength: 8)
            if hunk.added > 0 {
                Text("+\(hunk.added)").foregroundStyle(palette.signal)
            }
            if hunk.removed > 0 {
                Text("−\(hunk.removed)").foregroundStyle(palette.danger)
            }
        }
        .font(HorusStyle.metadataFont.weight(.semibold))
        .padding(.horizontal, 11)
        .padding(.vertical, 7)
        .frame(maxWidth: .infinity, minHeight: 32, alignment: .leading)
        .background(palette.accentSoft.opacity(0.45))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(hunk.title), \(hunk.added) additions, \(hunk.removed) removals"
        )
    }

    private var codeLine: some View {
        HStack(alignment: .top, spacing: 0) {
            gutter(row.oldNumber)
            gutter(row.newNumber)
            Text(marker)
                .font(HorusStyle.metadataFont.weight(.bold))
                .foregroundStyle(markerColor)
                .frame(width: 24)
            Text(verbatim: row.text.isEmpty ? " " : row.text)
                .font(HorusStyle.metadataFont)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.trailing, 10)
        }
        .frame(maxWidth: .infinity, minHeight: 23, alignment: .leading)
        .background(background)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    private func gutter(_ number: Int?) -> some View {
        Text(number.map(String.init) ?? "")
            .font(HorusStyle.metadataFont)
            .monospacedDigit()
            .foregroundStyle(palette.muted)
            .frame(width: 30, alignment: .trailing)
            .padding(.trailing, 5)
            .frame(maxHeight: .infinity)
            .background(palette.canvas.opacity(0.42))
            .overlay(alignment: .trailing) {
                Rectangle().fill(palette.line.opacity(0.6)).frame(width: 0.5)
            }
    }

    private var marker: String {
        switch row.kind {
        case .addition: "+"
        case .removal: "−"
        case .context: " "
        case .metadata: "·"
        case .hunk: ""
        }
    }

    private var markerColor: Color {
        switch row.kind {
        case .addition: palette.signal
        case .removal: palette.danger
        case .context, .metadata, .hunk: palette.muted
        }
    }

    private var background: Color {
        switch row.kind {
        case .addition: palette.signal.opacity(0.14)
        case .removal: palette.danger.opacity(0.14)
        case .context: palette.panel
        case .metadata: palette.raised.opacity(0.72)
        case .hunk: .clear
        }
    }

    private var accessibilityLabel: String {
        let location: String
        switch (row.oldNumber, row.newNumber) {
        case let (old?, new?): location = "old line \(old), new line \(new)"
        case let (old?, nil): location = "old line \(old)"
        case let (nil, new?): location = "new line \(new)"
        default: location = "metadata"
        }
        let change: String
        switch row.kind {
        case .addition: change = "Added"
        case .removal: change = "Removed"
        case .context: change = "Context"
        case .metadata: change = "Metadata"
        case .hunk: change = "Hunk"
        }
        return "\(change), \(location): \(row.text)"
    }
}

private struct DiffListRowModifier: ViewModifier {
    let topPadding: CGFloat
    let bottomPadding: CGFloat

    func body(content: Content) -> some View {
        content
            .padding(.top, topPadding)
            .padding(.bottom, bottomPadding)
            .listRowInsets(EdgeInsets(top: 0, leading: 14, bottom: 0, trailing: 14))
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
    }
}

private extension View {
    func diffListRow(topPadding: CGFloat = 0, bottomPadding: CGFloat = 0) -> some View {
        modifier(DiffListRowModifier(topPadding: topPadding, bottomPadding: bottomPadding))
    }
}

private extension UnifiedDiffFile {
    var name: String { path.split(separator: "/").last.map(String.init) ?? path }

    var parentPath: String? {
        let components = path.split(separator: "/")
        guard components.count > 1 else { return nil }
        return components.dropLast().joined(separator: "/")
    }
}
