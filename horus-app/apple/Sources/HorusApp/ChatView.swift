import Foundation
import SwiftUI
import SwiftStreamingMarkdown
import CoreText
@preconcurrency import AVFoundation
import UIKit

extension MountedWidget {
    var glyph: HorusGlyph {
        widget.symbol.map { HorusSymbol.glyph(for: $0) } ?? .squaresFour
    }

}
struct ChatView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var composerHeight: CGFloat = 0
    @State private var isAtBottom = true
    @State private var scrollToBottomRequest = 0
    @State private var presentedWidget: MountedWidget?
    @State private var showsChatAgentSettings = false
    @State private var hasEntered = false
    @State private var transcriptPresentationID = UUID()

    var body: some View {
        @Bindable var model = model
        ZStack(alignment: .bottom) {
            TranscriptView(
                bottomInset: composerHeight,
                isAtBottom: $isAtBottom,
                scrollToBottomRequest: scrollToBottomRequest
            )
            .id(transcriptPresentationID)
            ComposerView()
                .onGeometryChange(for: CGFloat.self) { geometry in
                    geometry.size.height
                } action: { height in
                    composerHeight = height
                }
                .zIndex(1)
            if !isAtBottom {
                Button("Scroll to latest", glyph: .arrowDown) {
                    scrollToBottomRequest += 1
                }
                .labelStyle(.iconOnly)
                .buttonStyle(HorusIconButtonStyle())
                .padding(.bottom, composerHeight + 12)
                .help("Scroll to latest")
                .zIndex(2)
            }
        }
        .scaleEffect(hasEntered || reduceMotion ? 1 : 0.985)
        .opacity(hasEntered ? 1 : 0)
        .onAppear {
            // SwiftUI can retain a navigation destination after it is popped. Give every
            // presentation a fresh scroll state even when the same session is reopened.
            transcriptPresentationID = UUID()
            withAnimation(reduceMotion ? .easeOut(duration: 0.12) : .smooth(duration: 0.28)) {
                hasEntered = true
            }
        }
        .onChange(of: model.selectedSessionID) {
            transcriptPresentationID = UUID()
            isAtBottom = true
        }
        .navigationTitle(chatTitle)
        .toolbarTitleDisplayMode(.inline)
        .toolbar {
            // Title changes animate glyphs, so the principal title must be a view the app
            // owns rather than the system's opaque navigation title.
            ToolbarItem(placement: .principal) {
                VStack(spacing: HorusSpace.xxs) {
                    HorusTitleText(title: chatTitle)
                        .font(HorusStyle.titleFont)
                        .lineLimit(1)
                    if !chatSubtitle.isEmpty {
                        Text(chatSubtitle)
                            .font(HorusStyle.captionFont)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                .accessibilityElement(children: .combine)
            }
            // One item holding both, so the spacing is this stack's rather than the bar's
            // between two items. The 44pt targets still touch; only the slack goes.
            ToolbarItem(placement: .primaryAction) {
                HStack(spacing: 0) {
                    newChatButton
                    ChatOptionsMenu(
                        presentedWidget: $presentedWidget,
                        showsAgentSettings: $showsChatAgentSettings
                    )
                }
            }
        }
        .sheet(item: $model.presentedPreview, content: PreviewTranscriptSheet.init)
        .sheet(item: $presentedWidget, content: FrontendWidgetSheet.init)
        .sheet(isPresented: $showsChatAgentSettings) {
            NavigationStack {
                AgentSettingsView(scope: .currentChat)
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Done") { showsChatAgentSettings = false }
                        }
                    }
            }
        }
    }

    /// Starting a chat in the folder you are already in belongs with the other page-level
    /// actions, not in the composer beside the controls that shape the message being written.
    private var newChatButton: some View {
        Button(action: model.openNewSessionInCurrentWorkspace) {
            toolbarGlyph(.notePencil)
        }
        .disabled(model.workspace == nil || !model.canCreateSession)
        .accessibilityLabel("New chat in this folder")
        .tint(.primary)
        .help("New chat in this folder")
    }

    /// A bare glyph is a 16pt target; toolbar buttons pad out to a full one the way every
    /// other icon button in the app does.
    private func toolbarGlyph(_ glyph: HorusGlyph) -> some View {
        HorusIcon(glyph, foreground: .primary)
            .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
            .contentShape(Rectangle())
    }

    private var workspaceName: String {
        guard let path = model.workspace?.path else { return "" }
        return path.split { $0 == "/" || $0 == "\\" }.last.map(String.init) ?? path
    }

    private var chatTitle: String {
        model.currentSessionTitle
    }

    private var chatSubtitle: String {
        [workspaceName, model.gatewayMachineName]
            .filter { !$0.isEmpty }
            .joined(separator: " • ")
    }
}

private struct ChatOptionsMenu: View {
    @Environment(AppModel.self) private var model
    @Binding var presentedWidget: MountedWidget?
    @Binding var showsAgentSettings: Bool

    var body: some View {
        Menu {
            Section(model.workspace?.path ?? "No chat selected") {
                if let git = model.gitStatus, !git.currentBranch.isEmpty {
                    Menu {
                        ForEach(git.branches, id: \.self) { branch in
                            Button {
                                model.switchGitBranch(to: branch)
                            } label: {
                                HorusLabel(
                                    title: branch,
                                    glyph: branch == git.currentBranch ? .check : .gitBranch
                                )
                            }
                            .disabled(branch == git.currentBranch)
                        }
                    } label: {
                        HorusLabel(
                            title: git.currentBranch,
                            glyph: .gitBranch
                        )
                    }
                    .disabled(model.isSwitchingGitBranch || !model.canModifySelectedSession)
                }
                Button { model.showFiles() } label: {
                    HorusLabel(
                        title: "Files",
                        glyph: .fileMagnifyingGlass
                    )
                }
                .disabled(model.selectedSessionID == nil || !model.connectionState.isReady)
                if let path = model.workspace?.path {
                    Button { copyToPasteboard(path) } label: {
                        HorusLabel(
                            title: "Copy workspace path",
                            glyph: .copy
                        )
                    }
                }
            }
            Section {
                Button {
                    showsAgentSettings = true
                } label: {
                    HorusLabel(
                        title: "Chat agent settings",
                        glyph: .slidersHorizontal
                    )
                }
                .disabled(model.selectedSessionID == nil || model.agentSnapshot == nil)
                ForEach(model.chatMenuWidgets) { widget in
                    Button {
                        activate(widget)
                    } label: {
                        HorusLabel(
                            title: widget.widget.text,
                            glyph: widget.glyph
                        )
                    }
                    .disabled(widget.widget.content == nil && widget.widget.action == nil)
                }
                Button {
                    model.startCronSetup()
                } label: {
                    HorusLabel(
                        title: "Schedule as a task…",
                        glyph: .calendarDots
                    )
                }
                .disabled(!model.canStartCronSetup)
                Button {
                    model.openNewSession()
                } label: {
                    HorusLabel(
                        title: "New chat in another folder…",
                        glyph: .folderPlus
                    )
                }
                .disabled(!model.canCreateSession)
            }
            if let session = model.selectedSession {
                Section {
                    Button {
                        model.beginRenamingSession(session)
                    } label: {
                        HorusLabel(title: "Rename chat", glyph: .pencilSimple)
                    }
                    .disabled(!model.canRenameSession)
                    Button(role: .destructive) {
                        model.beginDeletingSession(session)
                    } label: {
                        HorusLabel(title: "Delete chat", glyph: .trash)
                    }
                    .disabled(!model.canRenameSession)
                }
            }
        } label: {
            HorusIcon(.dotsThree, foreground: .primary)
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                .contentShape(Rectangle())
        }
        .labelStyle(.titleAndIcon)
        .menuIndicator(.hidden)
        .accessibilityLabel("Chat options")
        .tint(.primary)
        .help("Chat options")
    }

    private func activate(_ widget: MountedWidget) {
        if widget.widget.action != nil {
            model.submitWidget(widget)
        }
        if widget.widget.content != nil {
            presentedWidget = widget
        }
    }
}
