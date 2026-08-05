import SwiftUI
import Accessibility

private let debugStartsOnDetail: Bool = {
    #if DEBUG
    return ProcessInfo.processInfo.environment["HORUS_PAGE"] != nil
    #else
    return false
    #endif
}()

struct AppShell: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif
    @State private var columnVisibility = NavigationSplitViewVisibility.all
    @State private var compactColumn = debugStartsOnDetail ? NavigationSplitViewColumn.detail : .sidebar

    var body: some View {
        @Bindable var model = model
        ZStack(alignment: .top) {
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
                    SidebarView(showDetail: showDetail)
                        .navigationSplitViewColumnWidth(min: 230, ideal: 272, max: 340)
                } detail: {
                    destination
                }
                .navigationSplitViewStyle(.balanced)
                .inspector(isPresented: $model.showsInspector) {
                    ArtifactInspector()
                        .overlay(alignment: .top) {
                            #if os(iOS)
                            if horizontalSizeClass == .compact { AppToastOverlay() }
                            #endif
                        }
                }
                .sheet(isPresented: $model.showsPairing) {
                    PairingView(canCancel: true)
                        .frame(maxWidth: 560)
                        .padding(24)
                        .overlay(alignment: .top) { AppToastOverlay() }
                        .presentationDetents([.medium, .large])
                }
                .sheet(isPresented: $model.showsWorkspaceBrowser) {
                    WorkspaceBrowserView()
                        .frame(idealWidth: 520, idealHeight: 620)
                        .overlay(alignment: .top) { AppToastOverlay() }
                        .presentationDetents([.medium, .large])
                }
            }
            AppToastOverlay().zIndex(10)
        }
        .preferredColorScheme(preferredColorScheme)
        .onChange(of: chatIsVisible, initial: true) { _, visible in
            model.setChatVisible(visible)
        }
        .onChange(of: model.toast?.id) { _, _ in
            guard let toast = model.toast else { return }
            AccessibilityNotification.Announcement(
                "\(toast.tone.title): \(toast.message)"
            ).post()
        }
        .onChange(of: scenePhase) { _, newPhase in
            model.setSceneActive(newPhase == .active)
        }
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

    private func showDetail() {
        #if os(iOS)
        // Back can reveal the compact sidebar without updating this binding. Reassert the
        // visible column first so a second sidebar selection still produces a transition.
        if horizontalSizeClass == .compact, compactColumn == .detail {
            compactColumn = .sidebar
            Task { @MainActor in
                await Task.yield()
                compactColumn = .detail
            }
            return
        }
        #endif
        compactColumn = .detail
    }

    private var chatIsVisible: Bool {
        guard !model.accounts.isEmpty,
              model.destination == .chat,
              !model.showsPairing,
              !model.showsWorkspaceBrowser
        else { return false }
        #if os(iOS)
        return horizontalSizeClass != .compact
            || compactColumn == .detail && !model.showsInspector
        #else
        return true
        #endif
    }
}

private struct AppToastOverlay: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Group {
            if let toast = model.toast {
                AppToastView(toast: toast, dismiss: model.dismissToast)
                    .transition(
                        reduceMotion
                            ? .opacity
                            : .move(edge: .top).combined(with: .opacity)
                    )
            }
        }
        .frame(maxWidth: 520)
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .allowsHitTesting(model.toast != nil)
        .animation(
            reduceMotion ? .easeOut(duration: 0.12) : .smooth(duration: 0.28),
            value: model.toast?.id
        )
    }
}

private struct AppToastView: View {
    @Environment(\.horusPalette) private var palette
    let toast: AppToast
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            HStack(alignment: .top, spacing: 10) {
                HorusIcon(
                    systemName: toast.tone.systemImage,
                    size: 18,
                    foreground: toast.tone.color(in: palette)
                )
                VStack(alignment: .leading, spacing: 2) {
                    Text(toast.tone.title)
                        .font(HorusStyle.controlFont.weight(.semibold))
                        .foregroundStyle(toast.tone.color(in: palette))
                    Text(toast.message)
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(.primary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(toast.tone.title): \(toast.message)")

            Button(action: dismiss) {
                HorusIcon(systemName: "xmark", size: 14, foreground: palette.muted)
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Dismiss notification")
        }
        .padding(.leading, 16)
        .padding(.trailing, 6)
        .padding(.vertical, 10)
        .horusGlass(in: HorusStyle.cardShape)
        .shadow(color: .black.opacity(0.20), radius: 18, y: 8)
    }
}

private extension ToastTone {
    var title: String {
        switch self {
        case .info: "Notice"
        case .success: "Done"
        case .warning: "Attention"
        case .error: "Error"
        }
    }

    var systemImage: String {
        switch self {
        case .info: "info.circle"
        case .success: "checkmark.circle"
        case .warning: "exclamationmark.triangle"
        case .error: "xmark.circle"
        }
    }

    func color(in palette: HorusPalette) -> Color {
        switch self {
        case .info: palette.accent
        case .success: palette.signal
        case .warning: palette.warning
        case .error: palette.danger
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
                                HorusLabel(title: entry.name, systemImage: "folder")
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
                            HorusLabel(
                                title: error,
                                systemImage: "exclamationmark.triangle",
                                iconColor: palette.danger
                            )
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
                    Button("Parent folder", systemImage: "arrow.up") { onParent(parent) }
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
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.horusPalette) private var palette
    let showDetail: () -> Void
    @State private var collapsedWorkspaces: Set<String> = []
    @State private var sessionToRename: SessionRecord?
    @State private var renameDraft = ""
    @State private var sessionToDelete: SessionRecord?
    @State private var showsConnectionDetails = false

    var body: some View {
        ScrollView {
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
                    Button {
                        showsConnectionDetails = true
                    } label: {
                        Image(systemName: "circle.fill")
                            .font(.system(size: 8))
                            .foregroundStyle(
                                model.connectionState.isReady ? palette.signal : palette.danger
                            )
                            .symbolEffect(
                                .pulse.byLayer,
                                options: .repeat(.continuous),
                                isActive: !reduceMotion
                            )
                            .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Gateway connection")
                    .accessibilityValue(model.connectionState.label)
                    .help("Gateway: \(model.connectionState.label)")
                    .popover(isPresented: $showsConnectionDetails) { connectionDetails }
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
                    navigationButton("Chats", destination: .chat)
                        .padding(.horizontal, 12)

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
            .frame(maxWidth: .infinity)
        }
        .font(HorusStyle.bodyFont)
        // The split view paints its own system background over the app backdrop.
        .background { HorusBackdrop() }
        .safeAreaInset(edge: .bottom) {
            HStack {
                Button {
                    model.openNewSession()
                    model.destination = .chat
                    showDetail()
                } label: {
                    HorusLabel(title: "New chat", systemImage: "square.and.pencil")
                        .font(HorusStyle.controlFont)
                }
                .horusProminentButton()
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .disabled(!model.canCreateSession)
                .help("New chat")
                Spacer()
                Button {
                    model.destination = .profile
                    showDetail()
                } label: {
                    HorusIcon(systemName: "gearshape")
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

    private var connectionDetails: some View {
        VStack(spacing: 10) {
            Text(model.connectionState.label)
                .font(HorusStyle.controlFont.weight(.semibold))
                .foregroundStyle(model.connectionState.isReady ? palette.signal : palette.danger)

            if let account = model.selectedAccount {
                Text(account.displayName)
                Text(account.endpoint.rawValue)
                    .font(HorusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
            } else {
                Text("No gateway selected")
                    .foregroundStyle(palette.muted)
            }

            if case .failed(let message) = model.connectionState {
                Text(message)
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.danger)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity, alignment: .center)
            }

            if !model.connectionState.isReady {
                Divider()
                Button {
                    showsConnectionDetails = false
                    model.reconnect()
                } label: {
                    Label("Retry connection", systemImage: "arrow.clockwise")
                }
                .disabled(model.selectedAccount == nil)
                Button {
                    showsConnectionDetails = false
                    model.repairSelectedGateway()
                } label: {
                    Label("Repair pairing", systemImage: "link")
                }
                .disabled(model.selectedAccount == nil)
            }
        }
        .multilineTextAlignment(.center)
        .padding(16)
        .frame(width: 280)
        .presentationCompactAdaptation(.popover)
    }

    private func navigationButton(_ title: String, destination: AppDestination) -> some View {
        Button {
            model.destination = destination
            showDetail()
        } label: {
            HorusLabel(
                title: title,
                systemImage: destination.systemImage,
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
            HStack(spacing: 0) {
                Button {
                    expansionBinding(for: group.id).wrappedValue.toggle()
                } label: {
                    HStack(spacing: 6) {
                        HorusIcon(systemName: "folder", foreground: palette.muted)
                        Text(group.name)
                            .font(HorusStyle.controlFont)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        HorusIcon(
                            systemName: collapsedWorkspaces.contains(group.id)
                                ? "chevron.right"
                                : "chevron.down",
                            size: 12,
                            foreground: palette.muted
                        )
                    }
                    .frame(
                        maxWidth: .infinity,
                        minHeight: HorusStyle.iconButtonSize,
                        alignment: .leading
                    )
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityValue(
                    collapsedWorkspaces.contains(group.id) ? "Collapsed" : "Expanded"
                )
                .help(group.path)

                Button {
                    model.chooseWorkspace(group.path)
                    model.destination = .chat
                    showDetail()
                } label: {
                    HorusLabel(
                        title: "New chat in \(group.name)",
                        systemImage: "square.and.pencil",
                        iconColor: palette.muted,
                        iconSize: 14
                    )
                    .labelStyle(.iconOnly)
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(!model.canCreateSession)
                .help("New chat in \(group.path)")
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            if !collapsedWorkspaces.contains(group.id) {
                ForEach(group.sessions) { session in
                    sessionRow(session)
                }
            }
        }
    }

    private func sessionRow(_ session: SessionRecord) -> some View {
        let isSelected = session.sessionId == model.selectedSessionID
        let isUnread = model.unreadSessionIDs.contains(session.sessionId)
        let activityValue: String
        switch session.activity.state {
        case .running:
            activityValue = "In progress"
        case .awaitingApproval:
            activityValue = "Awaiting approval"
        case .idle:
            activityValue = isUnread ? "Finished, unread" : ""
        }
        return HStack(spacing: 4) {
            Button {
                model.openSession(session.sessionId)
                model.destination = .chat
                showDetail()
            } label: {
                HStack(spacing: 8) {
                    Text(session.displayTitle)
                        .fontWeight(isSelected ? .semibold : nil)
                        .lineLimit(1)
                        .foregroundStyle(isSelected ? palette.accent : .primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    SessionActivityIndicator(
                        state: session.activity.state,
                        isUnread: isUnread
                    )
                }
                .frame(minHeight: HorusStyle.iconButtonSize)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!model.canOpenSession && session.sessionId != model.selectedSessionID)
            .accessibilityValue(activityValue)
            .accessibilityAddTraits(isSelected ? .isSelected : [])

            if session.pinned {
                HorusIcon(systemName: "pin", size: 12, foreground: palette.accent)
            }

            Menu {
                Button {
                    model.setSessionPinned(session, pinned: !session.pinned)
                } label: {
                    Label(
                        session.pinned ? "Unpin" : "Pin",
                        systemImage: session.pinned ? "pin.slash" : "pin"
                    )
                }
                Button {
                    renameDraft = session.displayTitle
                    sessionToRename = session
                } label: {
                    Label("Rename", systemImage: "pencil")
                }
                Divider()
                Button(role: .destructive) {
                    sessionToDelete = session
                } label: {
                    Label("Delete", systemImage: "trash")
                }
            } label: {
                Image(systemName: "ellipsis")
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
            }
            .labelStyle(.titleAndIcon)
            .buttonStyle(.plain)
            .menuIndicator(.hidden)
            .accessibilityLabel("Chat actions")
            .help("Chat actions")
        }
        .padding(.horizontal, 8)
        .frame(minHeight: HorusStyle.iconButtonSize)
        .background(
            isSelected ? palette.accentSoft.opacity(0.55) : .clear,
            in: HorusStyle.controlShape
        )
        .overlay {
            HorusStyle.controlShape.stroke(
                isSelected ? palette.accent.opacity(0.5) : .clear,
                lineWidth: HorusStyle.borderWidth
            )
            .allowsHitTesting(false)
        }
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

private struct SessionActivityIndicator: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let state: SessionActivityState
    let isUnread: Bool

    var body: some View {
        Group {
            switch state {
            case .running:
                TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: reduceMotion)) { context in
                    let progress = context.date.timeIntervalSinceReferenceDate
                        .truncatingRemainder(dividingBy: 0.9) / 0.9
                    Circle()
                        .trim(from: 0.08, to: 0.76)
                        .stroke(
                            palette.accent,
                            style: StrokeStyle(lineWidth: 1.7, lineCap: .round)
                        )
                        .rotationEffect(.degrees(reduceMotion ? -90 : progress * 360 - 90))
                }
                .frame(width: 11, height: 11)
            case .awaitingApproval:
                Circle()
                    .trim(from: 0.08, to: 0.76)
                    .stroke(
                        palette.warning,
                        style: StrokeStyle(lineWidth: 1.7, lineCap: .round)
                    )
                    .rotationEffect(.degrees(-90))
                    .frame(width: 11, height: 11)
            case .idle:
                if isUnread {
                    Circle()
                        .fill(palette.accent)
                        .frame(width: 7, height: 7)
                        .frame(width: 11, height: 11)
                } else {
                    Color.clear.frame(width: 11, height: 11)
                }
            }
        }
        .accessibilityHidden(true)
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
                        Button("Close", systemImage: "xmark") {
                            model.showsPairing = false
                            dismiss()
                        }
                        .labelStyle(.iconOnly)
                        .buttonStyle(HorusIconButtonStyle())
                        .help("Close")
                    }
                }

                VStack(spacing: 12) {
                    HorusCard {
                        VStack(alignment: .leading, spacing: 18) {
                            VStack(alignment: .leading, spacing: 7) {
                                Text("Gateway address")
                                    .font(HorusStyle.controlFont)
                                HStack {
                                    TextField("wss://gateway.example", text: $model.pairingEndpoint)
                                        .textFieldStyle(.roundedBorder)
                                        .textContentType(.URL)
                                        .autocorrectionDisabled()
                                        .controlSize(.large)
                                    PasteButton(payloadType: String.self) { values in
                                        if let value = values.first {
                                            model.applyPairingSetup(value)
                                        }
                                    }
                                    .controlSize(.large)
                                    .accessibilityLabel("Paste pairing setup")
                                    .help("Paste pairing setup")
                                }
                            }
                            VStack(alignment: .leading, spacing: 7) {
                                Text("One-time code")
                                    .font(HorusStyle.controlFont)
                                SecureField("One-time code", text: $model.pairingCode)
                                    .textFieldStyle(.roundedBorder)
                                    .controlSize(.large)
                            }
                        }
                    }

                    Text("Cloud gateways use wss://. tcp:// is accepted only for localhost; direct remote gateways can use tls://.")
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.horizontal, 8)

                    if let error = model.pairingError {
                        HorusLabel(
                            title: error,
                            systemImage: "exclamationmark.triangle",
                            iconColor: palette.danger
                        )
                            .foregroundStyle(palette.danger)
                            .multilineTextAlignment(.center)
                    }
                }
                .frame(maxWidth: .infinity)
            }
        }
        .scrollIndicators(.hidden)
        .scrollBounceBehavior(.basedOnSize)
        .safeAreaInset(edge: .bottom) { pairAction }
        .onSubmit { model.pair() }
    }

    private var pairAction: some View {
        VStack(spacing: 14) {
            HorusLabel(
                title: "4-byte framed JSON · protocol v\(gatewayProtocolVersion)",
                systemImage: "checkmark.shield",
                iconColor: palette.muted
            )
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
            if model.connectionState == .connecting || model.connectionState == .authenticating {
                ProgressView().controlSize(.small)
            }
            Button("Pair gateway", action: model.pair)
                .horusProminentButton()
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                #if os(iOS)
                .buttonSizing(.flexible)
                #endif
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 16)
    }
}

extension String {
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
