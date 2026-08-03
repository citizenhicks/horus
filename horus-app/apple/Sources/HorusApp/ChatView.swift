import SwiftUI
import MarkdownView
#if os(macOS)
import AppKit
#elseif os(iOS)
import UIKit
#endif

struct ChatView: View {
    @Environment(AppModel.self) private var model
    @State private var composerHeight: CGFloat = 0

    var body: some View {
        ZStack(alignment: .bottom) {
            TranscriptView(bottomInset: composerHeight)
            ComposerView()
                .onGeometryChange(for: CGFloat.self) { geometry in
                    geometry.size.height
                } action: { height in
                    composerHeight = height
                }
            }
        .navigationTitle(model.transcript.isEmpty ? "Hello" : model.currentSessionTitle)
        #if os(iOS)
        .toolbarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            #if os(macOS)
            ToolbarItem(placement: .primaryAction) {
                ControlGroup {
                    inspectorButton
                    ChatOptionsMenu()
                }
                .controlGroupStyle(.navigation)
                .controlSize(.large)
                .tint(.primary)
            }
            #else
            ToolbarItemGroup(placement: .primaryAction) {
                inspectorButton
                ChatOptionsMenu()
            }
            #endif
        }
    }

    private var inspectorButton: some View {
        Button(action: model.toggleInspector) {
            HorusIcon(name: "panel-right", foreground: .primary)
        }
        .accessibilityLabel("Toggle artifact inspector")
        .tint(.primary)
        .help(model.showsInspector ? "Hide inspector" : "Show inspector")
    }
}

private struct ChatOptionsMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let totals = diffTotals(model.gitDiff)
        Menu {
            Section("Workspace") {
                Text(model.workspace?.path ?? "No chat selected")
                if let git = model.gitStatus, !git.currentBranch.isEmpty {
                    Text("Branch · \(git.currentBranch)")
                }
            }
            Section("Session") {
                Text(currentModelLabel)
                Text("\(model.contextTokens.formatted()) · \(model.contextFillPercent)%")
                ForEach(model.composerFooterWidgets) { widget in
                    if widget.widget.action != nil {
                        Button(compactWidgetValue(widget.widget.text)) {
                            model.submitWidget(widget, presentsPickerInInspector: true)
                        }
                    } else {
                        Text(compactWidgetValue(widget.widget.text))
                    }
                }
                Text("+\(totals.added) −\(totals.removed)")
                TimelineView(.periodic(from: .now, by: 1)) { timeline in
                    Text(formatDuration(model.generationElapsed(at: timeline.date)))
                }
                if model.currentUsage.inputTokens > 0 {
                    Text(cacheHit(model.currentUsage))
                }
            }
        } label: {
            HorusIcon(name: "ellipsis", foreground: .primary)
        }
        .menuIndicator(.hidden)
        .accessibilityLabel("Chat options")
        .tint(.primary)
        .help("Chat options")
    }

    private var currentModelLabel: String {
        guard let choice = model.modelChoices.first(where: { $0.route == model.selectedModelRoute }) else {
            return "Model"
        }
        return "\(choice.model) · \(choice.reasoningEffort?.capitalized ?? "Default")"
    }

}

private struct TranscriptView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var isAtBottom = true
    let bottomInset: CGFloat
    private let bottomID = "transcript-bottom"
    #if os(iOS)
    private let rowSpacing: CGFloat = 12
    private let contentPadding: CGFloat = 16
    #else
    private let rowSpacing: CGFloat = 16
    private let contentPadding: CGFloat = 24
    #endif

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: rowSpacing) {
                    ForEach(model.transcript) { entry in
                        TranscriptRow(entry: entry)
                            .id(entry.id)
                    }
                    Color.clear
                        .frame(height: max(1, bottomInset))
                        .id(bottomID)
                }
                .frame(maxWidth: 880)
                .frame(maxWidth: .infinity)
                .padding(contentPadding)
            }
            .background(palette.canvas)
            .id(model.selectedSessionID)
            .defaultScrollAnchor(.bottom, for: .initialOffset)
            .defaultScrollAnchor(.bottom, for: .sizeChanges)
            .scrollIndicators(.hidden)
            .overlay {
                if model.transcript.isEmpty { emptyState }
            }
            .onScrollGeometryChange(for: Bool.self) { geometry in
                geometry.visibleRect.maxY >= geometry.contentSize.height - 8
            } action: { _, atBottom in
                isAtBottom = atBottom
            }
            .overlay(alignment: .bottom) {
                if !isAtBottom {
                    Button("Scroll to latest", lucideIcon: "arrow-down") {
                        withAnimation(.easeOut(duration: 0.2)) {
                            proxy.scrollTo(bottomID, anchor: .bottom)
                        }
                    }
                    .labelStyle(.iconOnly)
                    .buttonStyle(HorusIconButtonStyle())
                    .padding(.bottom, bottomInset + 12)
                    .help("Scroll to latest")
                }
            }
            .onChange(of: model.transcript.last?.text) {
                guard isAtBottom || model.activeTurnID != nil else { return }
                proxy.scrollTo(bottomID, anchor: .bottom)
            }
        }
    }

    private var emptyState: some View {
        Text("𓂀")
            .font(.system(size: 58, weight: .regular, design: .serif))
            .foregroundStyle(palette.accent.opacity(0.5))
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(.bottom, bottomInset)
            .accessibilityHidden(true)
    }
}

private struct TranscriptRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var isHovered = false
    let entry: TranscriptEntry

    var body: some View {
        Group {
            if hasMessageActions {
                VStack(alignment: actionAlignment, spacing: 0) {
                    content
                    if hasInlineControls {
                        controls
                            .opacity(inlineControlsVisible ? 1 : 0)
                            .allowsHitTesting(inlineControlsVisible)
                            .accessibilityHidden(!inlineControlsVisible)
                    }
                }
                .frame(maxWidth: .infinity, alignment: frameAlignment)
                .animation(.easeOut(duration: 0.12), value: isHovered)
            } else {
                content
            }
        }
        .contentShape(Rectangle())
        .onHover { isHovered = $0 }
        .contextMenu { transcriptActions }
    }

    @ViewBuilder
    private var content: some View {
        switch entry.kind {
        case .user:
            HStack {
                Spacer(minLength: 42)
                Text(entry.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .background(palette.accentSoft, in: RoundedRectangle(cornerRadius: HorusStyle.cardRadius, style: .continuous))
            }
        case .assistant:
            MarkdownText(entry.text, parsesMarkdown: !entry.pending)
                .frame(maxWidth: .infinity, alignment: .leading)
        case .reasoning:
            MarkdownText(entry.text, parsesMarkdown: !entry.pending)
                .foregroundStyle(palette.muted)
            .padding(.leading, 14)
            .overlay(alignment: .leading) {
                Rectangle().fill(palette.line).frame(width: 2)
            }
        case .event, .error:
            if entry.format == "unified_diff" {
                Button { model.selectArtifact(entry.id) } label: {
                    EventCard(entry: entry)
                }
                .buttonStyle(.plain)
                .accessibilityHint("Opens the code diff")
            } else {
                EventCard(entry: entry)
            }
        }
    }

    private var controls: some View {
        HStack(spacing: 0) {
            MessageActionButton(title: "Copy", symbol: "copy") {
                copyToPasteboard(entry.text)
            }
            MessageActionButton(title: "Fork current chat", symbol: "git-fork") {
                model.forkSession()
            }
            .disabled(!model.canForkSession)
        }
    }

    @ViewBuilder
    private var transcriptActions: some View {
        if hasMessageActions {
            Button("Copy", lucideIcon: "copy") { copyToPasteboard(entry.text) }
            Button("Fork current chat", lucideIcon: "git-fork") { model.forkSession() }
                .disabled(!model.canForkSession)
        }
    }

    private var hasMessageActions: Bool {
        entry.kind == .user || entry.kind == .assistant
    }

    private var hasInlineControls: Bool {
        #if os(macOS)
        true
        #else
        entry.kind == .assistant
        #endif
    }

    private var inlineControlsVisible: Bool {
        entry.kind == .assistant || isHovered
    }

    private var actionAlignment: HorizontalAlignment {
        entry.kind == .user ? .trailing : .leading
    }

    private var frameAlignment: Alignment {
        entry.kind == .user ? .trailing : .leading
    }
}

private struct MessageActionButton: View {
    @Environment(\.horusPalette) private var palette
    @State private var isHovered = false
    let title: String
    let symbol: String
    let action: () -> Void

    var body: some View {
        Button(title, lucideIcon: symbol, action: action)
        .labelStyle(.iconOnly)
        .buttonStyle(.borderless)
        .font(HorusStyle.badgeFont)
        .foregroundStyle(isHovered ? palette.accent : palette.muted)
        .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
        .contentShape(Rectangle())
        .onHover { isHovered = $0 }
        .animation(.easeOut(duration: 0.12), value: isHovered)
        .help(title)
    }
}

private struct EventCard: View {
    @Environment(\.horusPalette) private var palette
    let entry: TranscriptEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(entry.format == "unified_diff" ? "CODE CHANGE" : entry.kind == .error ? "ERROR" : "EVENT")
                    .font(HorusStyle.metadataFont.weight(.bold))
                Spacer()
                if entry.pending { ProgressView().controlSize(.mini) }
                if entry.format == "unified_diff" { Text("Open").font(HorusStyle.controlFont) }
            }
            Text(entry.format == "unified_diff" ? diffSummary(entry.text) : entry.text)
                .font(entry.format == "unified_diff" ? HorusStyle.metadataFont : HorusStyle.bodyFont)
                .multilineTextAlignment(.leading)
                .lineLimit(entry.format == "unified_diff" ? 4 : nil)
                .textSelection(.enabled)
        }
        .foregroundStyle(entry.kind == .error ? palette.danger : palette.muted)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(HorusStyle.cardPadding)
        .background(
            entry.kind == .error ? palette.danger.opacity(0.09) : palette.raised,
            in: RoundedRectangle(cornerRadius: HorusStyle.cardRadius, style: .continuous)
        )
    }
}

private struct MarkdownText: View {
    let text: String
    let parsesMarkdown: Bool

    init(_ text: String, parsesMarkdown: Bool) {
        self.text = text
        self.parsesMarkdown = parsesMarkdown
    }

    var body: some View {
        if parsesMarkdown {
            MarkdownView(text.replacingOccurrences(
                of: #"\\dots\b"#,
                with: #"\\ldots"#,
                options: .regularExpression
            ))
                #if os(iOS)
                .markdownFontGroup(HorusMarkdownFonts())
                #endif
                .markdownMathRenderingEnabled()
                .markdownTableStyle(.github)
                .markdownBlockQuoteStyle(.github)
                .markdownCodeBlockStyle(.default(lightTheme: "xcode", darkTheme: "dark"))
                .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            Text(text).textSelection(.enabled)
        }
    }
}

#if os(iOS)
private struct HorusMarkdownFonts: MarkdownFontGroup {
    var h1: any CustomCTFontConvertible { Font.title3.weight(.semibold) }
    var h2: any CustomCTFontConvertible { Font.headline }
    var h3: any CustomCTFontConvertible { Font.subheadline.weight(.semibold) }
    var body: any CustomCTFontConvertible { HorusStyle.bodyFont }
    var blockQuote: any CustomCTFontConvertible { HorusStyle.bodyFont }
    var codeBlock: any CustomCTFontConvertible { Font.footnote.monospaced() }
    var tableBody: any CustomCTFontConvertible { HorusStyle.bodyFont }
    var inlineMath: any CustomCTFontConvertible { HorusStyle.bodyFont }
    var displayMath: any CustomCTFontConvertible { HorusStyle.bodyFont }
}
#endif

struct ComposerView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        VStack(spacing: 8) {
            ForEach(model.composerHeaderWidgets) { widget in
                FrontendWidgetView(widget: widget)
            }
            if let error = model.errorMessage {
                HStack(spacing: 8) {
                    HorusIcon(name: "triangle-alert", foreground: palette.danger)
                    Text(error).frame(maxWidth: .infinity, alignment: .leading)
                    Button("Dismiss") { model.errorMessage = nil }.buttonStyle(.borderless)
                }
                .font(HorusStyle.bodyFont)
                .foregroundStyle(palette.danger)
                .padding(8)
                .background(palette.danger.opacity(0.10), in: RoundedRectangle(cornerRadius: HorusStyle.controlRadius))
            }
            if let approval = model.pendingApproval {
                ApprovalView(approval: approval)
            }
            if let picker = model.pendingPicker {
                FrontendPickerView(picker: picker)
            }
            ComposerStack()
        }
        .frame(maxWidth: 880)
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 16)
        .padding(.bottom, 12)
    }
}

private struct ComposerStack: View {
    var body: some View {
        VStack(spacing: 4) {
            ComposerActivityView()
            ComposerSurface()
        }
    }
}

private struct ComposerSurface: View {
    @Environment(AppModel.self) private var model
    @State private var selection: TextSelection?

    var body: some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            TextField(
                "Ask Horus to inspect, explain, or change something…",
                text: $model.composer,
                selection: $selection,
                axis: .vertical
            )
            .textFieldStyle(.plain)
            .lineLimit(1...)
            .font(HorusStyle.bodyFont)
            .accessibilityLabel("Message")
            .onSubmit(submit)
            .onKeyPress(.return, phases: .down) { keyPress in
                if keyPress.modifiers.contains(.shift) {
                    insertLineBreak()
                } else {
                    submit()
                }
                return .handled
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            ComposerOptionsView()
                .padding(.horizontal, 16)
                .padding(.bottom, 12)
        }
        .horusGlass(
            in: RoundedRectangle(cornerRadius: HorusStyle.cardRadius, style: .continuous)
        )
        .shadow(color: .black.opacity(0.18), radius: 12, y: 6)
    }

    private func submit() {
        selection = nil
        model.sendMessage()
    }

    private func insertLineBreak() {
        var text = model.composer
        let range: Range<String.Index>
        if let selection, case .selection(let selectedRange) = selection.indices {
            range = selectedRange
        } else {
            range = text.endIndex..<text.endIndex
        }
        let offset = text.distance(from: text.startIndex, to: range.lowerBound)
        text.replaceSubrange(range, with: "\n")
        model.composer = text
        self.selection = TextSelection(
            insertionPoint: text.index(text.startIndex, offsetBy: offset + 1)
        )
    }
}

private struct ComposerActivityView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        let totals = diffTotals(model.gitDiff)
        GlassEffectContainer(spacing: 8) {
            HStack(spacing: 8) {
                #if os(macOS)
                if let workspace = model.workspace {
                    HorusIcon(name: "folder", foreground: palette.muted)
                        .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                        .help(workspace.path)
                        .accessibilityLabel("Workspace")
                        .accessibilityValue(workspace.path)
                }

                if let git = model.gitStatus, !git.currentBranch.isEmpty {
                    HorusIcon(name: "git-branch", foreground: palette.muted)
                        .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                        .help(git.currentBranch)
                        .accessibilityLabel("Git branch")
                        .accessibilityValue(git.currentBranch)
                }
                #endif

                ForEach(model.composerFooterWidgets) { widget in
                    FrontendWidgetView(widget: widget, presentsPickerInInspector: true)
                }
                if model.steeringQueued {
                    HorusBadge(text: "Steering queued", tone: "warning")
                }
                if totals.added > 0 || totals.removed > 0 {
                    Button { model.showInspector(.diff) } label: {
                        HStack(spacing: 6) {
                            Text("+\(totals.added)").foregroundStyle(palette.signal)
                            Text("−\(totals.removed)").foregroundStyle(palette.danger)
                        }
                        .font(HorusStyle.badgeFont)
                        .padding(.horizontal, 11)
                        .frame(height: HorusStyle.badgeHeight)
                        .horusGlass(in: Capsule(), interactive: true)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Code changes")
                    .accessibilityValue("\(totals.added) additions, \(totals.removed) deletions")
                    .accessibilityHint("Opens the latest code diff")
                }

                if model.turnStartedAt != nil || model.completedGenerationTime >= 1 {
                    TimelineView(.periodic(from: .now, by: 1)) { timeline in
                        let elapsed = model.generationElapsed(at: timeline.date)
                        if elapsed >= 1 {
                            HorusBadge(text: formatDuration(elapsed))
                                .accessibilityLabel("Total generation time")
                                .accessibilityValue(formatDuration(elapsed))
                        }
                    }
                }

                if model.contextFillPercent > 0 {
                    ZStack {
                        Circle().stroke(palette.line.opacity(0.55), lineWidth: 2)
                        Circle()
                            .trim(from: 0, to: model.contextFillFraction)
                            .stroke(palette.accent, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                            .rotationEffect(.degrees(-90))
                    }
                    .frame(width: 12, height: 12)
                    .frame(width: HorusStyle.badgeHeight, height: HorusStyle.badgeHeight)
                    .horusGlass(in: Circle())
                    .accessibilityLabel("Context used")
                    .accessibilityValue("\(model.contextFillPercent) percent")
                }
            }
            .frame(minHeight: HorusStyle.iconButtonSize)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .contain)
    }

}

private struct ComposerOptionsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        HStack(spacing: 8) {
            modelMenu
            approvalMenu
            Spacer()
            actionButtons
        }
    }

    private var modelMenu: some View {
        Menu {
            Section("Model") { modelMenuContent }
            Section("Reasoning") { reasoningMenuContent }
        } label: {
            HorusMenuLabel(text: modelLabel)
        }
        .buttonStyle(.plain)
        .frame(minHeight: HorusStyle.iconButtonSize)
        .accessibilityLabel("Model and reasoning")
        .accessibilityValue(modelLabel)
    }

    private var approvalMenu: some View {
        Menu { approvalMenuContent } label: {
            HorusLabel(title: approvalLabel, icon: approvalSymbol, iconColor: approvalForeground)
                .labelStyle(.iconOnly)
                .foregroundStyle(approvalForeground)
        }
        .buttonStyle(.plain)
        .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
        .disabled(model.agentDraft == nil)
        .help(approvalLabel)
        .accessibilityLabel("Approval policy")
        .accessibilityValue(approvalLabel)
    }

    @ViewBuilder
    private var modelMenuContent: some View {
        ForEach(distinctModels, id: \.route) { choice in
            Button("\(choice.group) · \(choice.model)") {
                let effort = currentChoice?.reasoningEffort
                let target = model.modelChoices.first {
                    $0.group == choice.group && $0.model == choice.model && $0.reasoningEffort == effort
                } ?? choice
                model.selectModel(target.route)
            }
        }
    }

    @ViewBuilder
    private var reasoningMenuContent: some View {
        ForEach(reasoningChoices, id: \.route) { choice in
            Button(choice.reasoningEffort?.capitalized ?? "Default") {
                model.selectModel(choice.route)
            }
        }
    }

    @ViewBuilder
    private var approvalMenuContent: some View {
        Button("Ask") { selectApproval(.on) }
        Button("Allow · no network") { selectApproval(.allow) }
        Button("Allow · network", role: .destructive) { selectApproval(.allowNetwork) }
    }

    @ViewBuilder
    private var actionButtons: some View {
        if model.activeTurnID != nil && !hasComposerText {
            Button("Stop", lucideIcon: "square") { model.interrupt() }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle(prominent: true))
                .help("Stop")
        } else {
            Button("Send", lucideIcon: "arrow-up") { model.sendMessage() }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle(prominent: true))
                .disabled(!model.connectionState.isReady || !hasComposerText)
                .help(model.activeTurnID == nil ? "Send" : "Send steering message")
        }
    }

    private var currentChoice: ModelChoice? {
        model.modelChoices.first { $0.route == model.selectedModelRoute }
    }

    private var distinctModels: [ModelChoice] {
        var seen = Set<String>()
        return model.modelChoices.filter { seen.insert("\($0.group)\u{0}\($0.model)").inserted }
    }

    private var reasoningChoices: [ModelChoice] {
        guard let currentChoice else { return [] }
        return model.modelChoices.filter {
            $0.group == currentChoice.group && $0.model == currentChoice.model
        }
    }

    private var approvalLabel: String {
        approvalPolicyLabel(model.agentDraft?.approval ?? .on)
    }

    private var approvalSymbol: String {
        approvalPolicySymbol(model.agentDraft?.approval ?? .on)
    }

    private var approvalForeground: Color {
        model.agentDraft?.approval == .allowNetwork ? palette.warning : palette.muted
    }

    private var modelLabel: String {
        guard let currentChoice else { return "Model" }
        return "\(currentChoice.model) · \(currentChoice.reasoningEffort?.capitalized ?? "Default")"
    }

    private var hasComposerText: Bool {
        !model.composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func selectApproval(_ policy: ApprovalPolicy) {
        model.setApprovalPolicy(policy)
    }
}

private struct ApprovalView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let approval: PendingApproval

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HorusLabel(title: "Approval required", icon: "hand", iconColor: palette.warning)
                .font(.headline)
                .foregroundStyle(palette.warning)
            Text(approval.reason).font(HorusStyle.bodyFont)
            ScrollView([.horizontal, .vertical]) {
                LazyVStack(alignment: .leading, spacing: 9) {
                    ForEach(approval.calls) { call in
                        VStack(alignment: .leading, spacing: 5) {
                            Text(call.name).font(HorusStyle.metadataFont.weight(.bold))
                            Text(call.arguments).font(HorusStyle.metadataFont).textSelection(.enabled)
                        }
                        .padding(10)
                        .background(palette.raised, in: RoundedRectangle(cornerRadius: HorusStyle.controlRadius))
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(call.name), arguments \(call.arguments)")
                    }
                }
            }
            .frame(maxHeight: 180)
            HStack {
                Button("Abort") { model.resolveApproval(.abort) }.buttonStyle(.borderless)
                Spacer()
                Button("Deny") { model.resolveApproval(.denied(rejection: "Denied in Horus App")) }
                    .buttonStyle(.glass)
                    .buttonBorderShape(.capsule)
                Button("Approve for session") { model.resolveApproval(.approvedForSession) }
                    .buttonStyle(.glass)
                    .buttonBorderShape(.capsule)
                Button("Approve once") { model.resolveApproval(.approved) }
                    .buttonStyle(.glassProminent)
                    .buttonBorderShape(.capsule)
            }
        }
        .padding(HorusStyle.cardPadding)
        .background(palette.warning.opacity(0.09), in: RoundedRectangle(cornerRadius: HorusStyle.cardRadius))
        .overlay {
            RoundedRectangle(cornerRadius: HorusStyle.cardRadius)
                .stroke(palette.warning.opacity(0.55), lineWidth: HorusStyle.borderWidth)
        }
    }
}

struct FrontendWidgetView: View {
    @Environment(AppModel.self) private var model
    let widget: MountedWidget
    var presentsPickerInInspector = false

    var body: some View {
        if widget.widget.action != nil {
            Button { model.submitWidget(widget, presentsPickerInInspector: presentsPickerInInspector) } label: {
                HorusBadge(
                    text: widgetText,
                    tone: widget.widget.tone,
                    symbol: presentsPickerInInspector ? "bot" : nil,
                    interactive: true
                )
            }
            .buttonStyle(.plain)
            .frame(minHeight: HorusStyle.iconButtonSize)
            .accessibilityLabel(widget.widget.text)
        } else {
            HorusBadge(
                text: widgetText,
                tone: widget.widget.tone,
                symbol: presentsPickerInInspector ? "bot" : nil
            )
                .frame(minHeight: HorusStyle.iconButtonSize)
                .accessibilityLabel(widget.widget.text)
        }
    }

    private var widgetText: String {
        guard presentsPickerInInspector else { return widget.widget.text }
        return widget.widget.text.split(separator: " ", maxSplits: 1).last.map(String.init) ?? widget.widget.text
    }
}

private struct FrontendPickerView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let picker: FrontendPickerPrompt

    var body: some View {
        VStack(alignment: .leading, spacing: 11) {
            Text(picker.title)
                .font(.headline)
            ForEach(picker.options) { option in
                Button { model.submitPickerOption(option) } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                        Text(option.label).fontWeight(.semibold)
                        Text(option.description)
                            .font(HorusStyle.bodyFont)
                            .foregroundStyle(palette.muted)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .horusPopoverCard()
    }
}

private func copyToPasteboard(_ text: String) {
    #if os(macOS)
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(text, forType: .string)
    #elseif os(iOS)
    UIPasteboard.general.string = text
    #endif
}

private func diffTotals(_ text: String) -> (added: Int, removed: Int) {
    text.split(separator: "\n", omittingEmptySubsequences: false).reduce(into: (0, 0)) { result, line in
        if line.hasPrefix("+") && !line.hasPrefix("+++") { result.0 += 1 }
        if line.hasPrefix("-") && !line.hasPrefix("---") { result.1 += 1 }
    }
}

private func formatDuration(_ interval: TimeInterval) -> String {
    let seconds = max(0, Int(interval))
    return Duration.seconds(seconds).formatted(.time(pattern: .minuteSecond(padMinuteToLength: 1)))
}

private func compactWidgetValue(_ text: String) -> String {
    text.split(separator: " ", maxSplits: 2).dropFirst().first.map(String.init) ?? text
}

private func approvalPolicyLabel(_ policy: ApprovalPolicy) -> String {
    switch policy {
    case .on: "Ask"
    case .allow: "Allow · no network"
    case .allowNetwork: "Allow · network"
    }
}

private func approvalPolicySymbol(_ policy: ApprovalPolicy) -> String {
    switch policy {
    case .on: "shield-question-mark"
    case .allow: "shield-check"
    case .allowNetwork: "globe-lock"
    }
}

private func diffSummary(_ text: String) -> String {
    let totals = diffTotals(text)
    return "\(diffTitle(text))  ·  +\(totals.added) −\(totals.removed)"
}
