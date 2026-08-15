import Foundation
import SwiftUI

/// A completed turn becomes one disclosure without changing how the same rows render live.
struct WorkedForGroupView: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @ScaledMetric(relativeTo: .body) private var summaryHeight = HorusStyle.rowRegular
    @State private var isExpanded = false
    let entries: [TranscriptEntry]
    let elapsedMs: UInt64?
    var onExpand: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.s) {
            Button {
                if !isExpanded { onExpand() }
                withAnimation(reduceMotion ? nil : .easeOut(duration: 0.16)) {
                    isExpanded.toggle()
                }
            } label: {
                HStack(spacing: HorusSpace.s) {
                    HorusIcon(.combine, size: HorusStyle.glyphInline, foreground: palette.muted)
                    Text(title)
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                        .lineLimit(1)
                    Spacer(minLength: HorusSpace.s)
                    HorusIcon(.caretRight, size: HorusStyle.glyphMark, foreground: palette.muted)
                        .rotationEffect(.degrees(isExpanded ? 90 : 0))
                        .animation(
                            reduceMotion ? nil : .snappy(duration: 0.18),
                            value: isExpanded
                        )
                }
                .frame(minHeight: summaryHeight)
                .contentShape(Rectangle())
            }
            .buttonStyle(.horusPlain)
            .accessibilityLabel(title)
            .accessibilityValue(isExpanded ? "Expanded" : "Collapsed")
            .accessibilityHint(isExpanded ? "Collapses the completed work" : "Shows the completed work")

            if isExpanded {
                TranscriptRowsView(
                    projection: TranscriptProjection(entries: entries),
                    rowSpacing: HorusSpace.s,
                    onExpandActivityGroup: onExpand
                )
            }
        }
    }

    private var title: String {
        let elapsed = TimeInterval(elapsedMs ?? 0) / 1_000
        return "Worked for \(formatDuration(elapsed))"
    }
}

/// A run of consecutive events behind one summary line, so a long turn costs one row until
/// the reader asks for more.
struct EventGroupView: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// One line, whatever it says. A count climbing from 1 to 47, an icon swapping, the
    /// waiting phrase taking the summary's place: none of it changes the row's height.
    /// Scaled rather than fixed, because `.body` grows with Dynamic Type and a hard 30pt
    /// clips it at accessibility sizes.
    @ScaledMetric(relativeTo: .body) private var summaryHeight = HorusStyle.rowRegular
    @State private var isExpanded = false
    let entries: [TranscriptEntry]
    let isActive: Bool
    /// The gap between two steps belongs to this row: rather than growing the transcript by a
    /// line that then has to disappear again, the summary hands its slot to the waiting line.
    var waiting: TranscriptWaitingPhrase?
    var onExpand: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.s) {
            // Files an event produced are the deliverable, not a detail, so they stay out.
            TranscriptFileCards(files: files)
            // The summary slot belongs to the run, not to its contents: while the run holds
            // the waiting phrase it draws the slot whether or not any step has named itself,
            // so naming one costs a crossfade rather than a row's worth of height.
            if !lines.isEmpty || waiting != nil {
                Button {
                    if !isExpanded { onExpand() }
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
                        ForEach(lines, id: \.presentationID) { entry in
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
                        .font(HorusStyle.bodyFont)
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
        .frame(minHeight: summaryHeight)
        .contentShape(Rectangle())
    }

    private var lines: [TranscriptEntry] {
        entries.filter(\.hasActivityLineContent)
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
                .font(HorusStyle.bodyFont)
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

/// The bottom of the transcript, as one view with one state.
///
/// The waiting line used to be a row that appeared and disappeared while a group header
/// separately took the phrase over, which meant two views trading a slot and a row's height
/// moving with them. The projection now says which state the tail is in; this draws it.
struct TranscriptTailView: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// `.body` scales with Dynamic Type, so a hard 30pt clips the line at accessibility
    /// sizes. The summary slot and this line share the metric and stay the same height.
    @ScaledMetric(relativeTo: .body) private var lineHeight = HorusStyle.rowRegular
    let slot: TranscriptWaitingSlot
    /// Owned rather than applied by the transcript: padding outside the condition reserves
    /// the gap while the line is absent, and the arriving row then lands 12pt low.
    let topSpacing: CGFloat

    var body: some View {
        Group {
            // The other cases belong to a row: its summary is its own, and the phrase, when a
            // row holds it, is drawn inside that row's header.
            if case .standaloneLine(let phrase) = slot {
                HStack(spacing: HorusSpace.s) {
                    HorusIcon(
                        .neuralNetwork,
                        size: HorusStyle.glyphInline,
                        foreground: palette.muted
                    )
                    TranscriptWaitingPhraseText(phrase: phrase)
                }
                .frame(maxWidth: .infinity, minHeight: lineHeight, alignment: .leading)
                .padding(.top, topSpacing)
                .transition(
                    reduceMotion ? .opacity : .opacity.combined(with: .offset(y: 8))
                )
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("Waiting for the model")
            }
        }
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
                        .font(HorusStyle.bodyFont)
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
        .font(HorusStyle.bodyFont)
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
