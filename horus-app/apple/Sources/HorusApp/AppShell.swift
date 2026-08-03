import SwiftUI

struct AppShell: View {
    @Environment(AppModel.self) private var model
    @State private var columnVisibility = NavigationSplitViewVisibility.all
    @State private var compactColumn = NavigationSplitViewColumn.sidebar

    var body: some View {
        @Bindable var model = model
        ZStack {
            HorusBackdrop()
            if model.accounts.isEmpty {
                PairingView(canCancel: false)
                    .frame(maxWidth: 620)
                    .padding(24)
            } else {
                NavigationSplitView(
                    columnVisibility: $columnVisibility,
                    preferredCompactColumn: $compactColumn
                ) {
                    SidebarView { compactColumn = .detail }
                        .navigationSplitViewColumnWidth(min: 230, ideal: 272, max: 340)
                } detail: {
                    destination
                }
                .navigationSplitViewStyle(.balanced)
                .inspector(isPresented: $model.showsInspector) {
                    ArtifactInspector()
                }
                .sheet(isPresented: $model.showsPairing) {
                    PairingView(canCancel: true)
                        .frame(maxWidth: 560)
                        .padding(24)
                        .presentationDetents([.medium, .large])
                }
                .sheet(isPresented: $model.showsWorkspaceBrowser) {
                    WorkspaceBrowserView()
                        .frame(idealWidth: 520, idealHeight: 620)
                        .presentationDetents([.medium, .large])
                }
            }
        }
        .preferredColorScheme(preferredColorScheme)
        .task { model.start() }
    }

    @ViewBuilder
    private var destination: some View {
        switch model.destination ?? .chat {
        case .chat: ChatView()
        case .gateway: GatewayView()
        case .agent: AgentSettingsView()
        case .providers: ProvidersView()
        case .cron: CronView()
        case .profile: ProfileView()
        }
    }

    private var preferredColorScheme: ColorScheme? {
        switch model.theme {
        case .system: nil
        case .dark: .dark
        case .light: .light
        }
    }

}

private struct WorkspaceBrowserView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.horusPalette) private var palette

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if let listing = model.directoryListing {
                    DirectoryBrowserHeader(
                        path: listing.path,
                        title: "Choose a workspace for the new chat",
                        parent: listing.parent,
                        onParent: model.loadDirectory
                    )
                    List {
                        ForEach(listing.entries) { entry in
                            Button { model.loadDirectory(entry.path) } label: {
                                HorusLabel(title: entry.name, icon: "folder")
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .listRowSeparator(.hidden)
                        }
                        if listing.entries.isEmpty && !model.isLoadingDirectories {
                            Text("No folders")
                                .foregroundStyle(palette.muted)
                                .listRowSeparator(.hidden)
                        }
                        if let error = model.directoryError ?? model.workspaceError {
                            HorusLabel(title: error, icon: "triangle-alert", iconColor: palette.danger)
                                .foregroundStyle(palette.danger)
                                .listRowSeparator(.hidden)
                        }
                    }
                    .listStyle(.plain)
                    .scrollContentBackground(.hidden)
                }
            }
            .font(HorusStyle.bodyFont)
            .overlay {
                if model.isLoadingDirectories { ProgressView() }
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        model.showsWorkspaceBrowser = false
                        dismiss()
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Choose") {
                        if let path = model.directoryListing?.path { model.chooseWorkspace(path) }
                    }
                    .disabled(
                        model.directoryListing?.parent == nil
                            || model.isChangingWorkspace
                    )
                }
            }
        }
    }
}

private struct DirectoryBrowserHeader: View {
    @Environment(\.horusPalette) private var palette
    let path: String
    let title: String
    let parent: String?
    let onParent: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(path)
                .font(HorusStyle.metadataFont.weight(.bold))
                .tracking(1)
                .foregroundStyle(palette.accent)
                .lineLimit(2)
            HStack {
                Text(title)
                    .font(HorusStyle.controlFont)
                Spacer()
                if let parent {
                    Button("Parent folder", lucideIcon: "folder-up") { onParent(parent) }
                        .labelStyle(.iconOnly)
                        .buttonStyle(HorusIconButtonStyle())
                        .help("Parent folder")
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }
}

private struct ArtifactInspector: View {
    var body: some View {
        ArtifactView()
            .inspectorColumnWidth(min: 320, ideal: 520, max: 840)
    }
}

struct SidebarView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let showDetail: () -> Void
    @State private var collapsedWorkspaces: Set<String> = []
    @State private var sessionToRename: SessionRecord?
    @State private var renameDraft = ""
    @State private var sessionToDelete: SessionRecord?

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Text("𓂀")
                    .font(.system(size: 27, weight: .regular, design: .serif))
                    .foregroundStyle(palette.accent)
                    .accessibilityHidden(true)
                Text("HORUS")
                    .font(.system(.subheadline, design: .serif, weight: .bold))
                    .tracking(1.4)
                Spacer()
                Menu {
                    Text(model.connectionState.label)
                    if let account = model.selectedAccount {
                        Text(account.displayName)
                        Text(account.endpoint.rawValue)
                    } else {
                        Text("No gateway selected")
                    }
                    if !model.connectionState.isReady {
                        Divider()
                        if case .failed(let message) = model.connectionState {
                            Text(message)
                        }
                        Button("Retry connection", lucideIcon: "refresh-cw", action: model.reconnect)
                            .disabled(model.selectedAccount == nil)
                        Button("Repair pairing", lucideIcon: "link") {
                            model.repairSelectedGateway()
                        }
                        .disabled(model.selectedAccount == nil)
                    }
                } label: {
                    Circle()
                        .fill(model.connectionState.isReady ? palette.signal : palette.danger)
                        .frame(width: 8, height: 8)
                        .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .accessibilityLabel("Gateway connection")
                .accessibilityValue(model.connectionState.label)
                .help("Gateway: \(model.connectionState.label)")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 16)
            .padding(.vertical, 10)

            VStack(alignment: .leading, spacing: 2) {
                navigationButton("Gateway", destination: .gateway)
                navigationButton("Providers", destination: .providers)
                navigationButton("Agent settings", destination: .agent)
                navigationButton("Cron", destination: .cron)
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 10)

            VStack(alignment: .leading, spacing: 0) {
                HorusLabel(
                    title: "Chats",
                    icon: "messages-square",
                    iconColor: model.destination == .chat ? palette.accent : Color.primary
                )
                    .font(HorusStyle.controlFont)
                    .foregroundStyle(model.destination == .chat ? palette.accent : Color.primary)
                    .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize, alignment: .leading)
                    .padding(.horizontal, 16)

                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        if model.sessions.isEmpty {
                            Text(model.connectionState.isReady ? "No chats yet" : model.connectionState.label)
                                .foregroundStyle(palette.muted)
                        }
                        ForEach(sessionGroups) { group in
                            workspaceGroup(group)
                        }
                    }
                    .padding(.horizontal, 16)
                }
            }
        }
        .font(HorusStyle.bodyFont)
        .safeAreaInset(edge: .bottom) {
            HStack {
                Button {
                    model.openNewSession()
                    model.destination = .chat
                    showDetail()
                } label: {
                    HorusLabel(title: "New chat", icon: "square-pen")
                        .font(HorusStyle.controlFont)
                }
                .buttonStyle(.glassProminent)
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .disabled(!model.canCreateSession)
                .help("New chat")
                Spacer()
                Button {
                    model.destination = .profile
                    showDetail()
                } label: {
                    HorusIcon(name: "settings")
                }
                .buttonStyle(HorusIconButtonStyle())
                .accessibilityLabel("Settings")
                .help("Settings")
            }
            .padding(12)
        }
        #if os(iOS)
        .toolbarVisibility(.hidden, for: .navigationBar)
        #endif
        .alert("Rename chat", isPresented: renamePresented) {
            TextField("Chat name", text: $renameDraft)
            Button("Cancel", role: .cancel) { sessionToRename = nil }
            Button("Rename") {
                if let sessionToRename { model.renameSession(sessionToRename, title: renameDraft) }
                sessionToRename = nil
            }
            .disabled(renameDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
        .confirmationDialog(
            "Delete this chat?",
            isPresented: deletePresented,
            titleVisibility: .visible
        ) {
            Button("Delete chat", role: .destructive) {
                if let sessionToDelete { model.deleteSession(sessionToDelete) }
                sessionToDelete = nil
            }
            Button("Cancel", role: .cancel) { sessionToDelete = nil }
        } message: {
            Text("This removes the chat from the gateway history.")
        }
    }

    private func navigationButton(_ title: String, destination: AppDestination) -> some View {
        Button {
            model.destination = destination
            showDetail()
        } label: {
            HorusLabel(
                title: title,
                icon: destination.symbol,
                iconColor: model.destination == destination ? palette.accent : Color.primary
            )
                .font(HorusStyle.controlFont)
                .foregroundStyle(model.destination == destination ? palette.accent : Color.primary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 4)
        .frame(minHeight: HorusStyle.iconButtonSize)
    }

    private var sessionGroups: [WorkspaceSessions] {
        Dictionary(grouping: model.sessions) {
            $0.sessionContext.workspaceId ?? $0.sessionContext.workspaceLabel ?? "workspace"
        }
        .map { id, sessions in
            let path = sessions.first?.sessionContext.workspaceLabel ?? "Workspace"
            let name = URL(fileURLWithPath: path).lastPathComponent.nonEmpty ?? path
            return WorkspaceSessions(
                id: id,
                name: name,
                path: path,
                sessions: sessions.sorted {
                    if $0.pinned != $1.pinned { return $0.pinned }
                    return $0.updatedAt > $1.updatedAt
                }
            )
        }
        .sorted {
            if $0.id == model.workspace?.id { return true }
            if $1.id == model.workspace?.id { return false }
            return ($0.sessions.first?.updatedAt ?? 0) > ($1.sessions.first?.updatedAt ?? 0)
        }
    }

    private func expansionBinding(for id: String) -> Binding<Bool> {
        Binding(
            get: { !collapsedWorkspaces.contains(id) },
            set: { expanded in
                if expanded { collapsedWorkspaces.remove(id) }
                else { collapsedWorkspaces.insert(id) }
            }
        )
    }

    private func workspaceGroup(_ group: WorkspaceSessions) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                expansionBinding(for: group.id).wrappedValue.toggle()
            } label: {
                HStack(spacing: 6) {
                    HorusIcon(name: "folder", foreground: palette.muted)
                    Text(group.name)
                        .font(HorusStyle.controlFont)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    HorusIcon(
                        name: collapsedWorkspaces.contains(group.id) ? "chevron-right" : "chevron-down",
                        size: 12,
                        foreground: palette.muted
                    )
                }
                .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(group.path)

            if !collapsedWorkspaces.contains(group.id) {
                ForEach(group.sessions) { session in
                    sessionRow(session)
                }
            }
        }
    }

    private func sessionRow(_ session: SessionRecord) -> some View {
        let isSelected = session.sessionId == model.selectedSessionID
        return HStack(spacing: 4) {
            Button {
                model.openSession(session.sessionId)
                model.destination = .chat
                showDetail()
            } label: {
                Text(session.displayTitle)
                    .lineLimit(1)
                    .foregroundStyle(.primary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!model.canOpenSession && session.sessionId != model.selectedSessionID)

            if session.pinned {
                HorusIcon(name: "pin", size: 12, foreground: palette.accent)
            }

            Menu {
                Button(session.pinned ? "Unpin" : "Pin", lucideIcon: session.pinned ? "pin-off" : "pin") {
                    model.setSessionPinned(session, pinned: !session.pinned)
                }
                .disabled(!isSelected)
                Button("Rename", lucideIcon: "pencil") {
                    renameDraft = session.displayTitle
                    sessionToRename = session
                }
                .disabled(!isSelected)
                Divider()
                Button("Delete", lucideIcon: "trash-2", role: .destructive) {
                    sessionToDelete = session
                }
                .disabled(!isSelected)
            } label: {
                HorusIcon(name: "ellipsis", size: 14)
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .menuIndicator(.hidden)
            .accessibilityLabel("Chat actions")
            .help("Chat actions")
        }
        .frame(minHeight: HorusStyle.iconButtonSize)
    }

    private var renamePresented: Binding<Bool> {
        Binding(
            get: { sessionToRename != nil },
            set: { if !$0 { sessionToRename = nil } }
        )
    }

    private var deletePresented: Binding<Bool> {
        Binding(
            get: { sessionToDelete != nil },
            set: { if !$0 { sessionToDelete = nil } }
        )
    }
}

private struct WorkspaceSessions: Identifiable {
    let id: String
    let name: String
    let path: String
    let sessions: [SessionRecord]
}

struct PairingView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.horusPalette) private var palette
    let canCancel: Bool

    var body: some View {
        @Bindable var model = model
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                HStack(alignment: .top) {
                    SectionHeading(
                        title: "Pair with a gateway",
                        detail: "Use the same address and one-time code on Mac, iPad, or iPhone."
                    )
                    Spacer()
                    if canCancel {
                        Button("Close", lucideIcon: "x") {
                            model.showsPairing = false
                            dismiss()
                        }
                        .labelStyle(.iconOnly)
                        .buttonStyle(HorusIconButtonStyle())
                        .help("Close")
                    }
                }

                HorusCard {
                    VStack(alignment: .leading, spacing: 18) {
                        VStack(alignment: .leading, spacing: 7) {
                            Text("Gateway address")
                                .font(HorusStyle.controlFont)
                            TextField("tls://gateway.example:7443", text: $model.pairingEndpoint)
                                .textFieldStyle(.roundedBorder)
                                .textContentType(.URL)
                                .autocorrectionDisabled()
                                .controlSize(.large)
                        }
                        VStack(alignment: .leading, spacing: 7) {
                            Text("One-time code")
                                .font(HorusStyle.controlFont)
                            SecureField("Pairing code", text: $model.pairingCode)
                                .textFieldStyle(.roundedBorder)
                                .controlSize(.large)
                        }
                        Text("Remote gateways require tls://. tcp:// is accepted only for localhost or a loopback address.")
                            .font(HorusStyle.bodyFont)
                            .foregroundStyle(palette.muted)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }

                if let error = model.pairingError {
                    HorusLabel(title: error, icon: "triangle-alert", iconColor: palette.danger)
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(palette.danger)
                }

                VStack(spacing: 14) {
                    HorusLabel(
                        title: "4-byte framed JSON · protocol v\(gatewayProtocolVersion)",
                        icon: "shield-check",
                        iconColor: palette.muted
                    )
                        .font(HorusStyle.metadataFont)
                        .foregroundStyle(palette.muted)
                    if model.connectionState == .connecting || model.connectionState == .authenticating {
                        ProgressView().controlSize(.small)
                    }
                    Button(action: model.pair) {
                        Text("Pair gateway")
                            #if os(iOS)
                            .frame(maxWidth: .infinity)
                            #endif
                    }
                    .buttonStyle(.glassProminent)
                    .buttonBorderShape(.capsule)
                    .controlSize(.large)
                }
                .frame(maxWidth: .infinity)
            }
        }
        .scrollIndicators(.hidden)
        .onSubmit { model.pair() }
    }
}

private extension String {
    var nonEmpty: String? {
        let trimmed = trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

private extension SessionRecord {
    var displayTitle: String {
        title?.nonEmpty ?? firstUserMessage?.nonEmpty ?? "Untitled chat"
    }
}
