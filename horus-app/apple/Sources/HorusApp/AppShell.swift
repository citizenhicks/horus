import SwiftUI
import Accessibility
import QuickLook

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
    #if os(iOS)
    @State private var sidebarIsOpen = !debugStartsOnDetail
    #endif

    var body: some View {
        @Bindable var model = model
        ZStack(alignment: .top) {
            if model.isAppLocked || model.appLockEnabled && scenePhase != .active {
                AppLockView()
            } else {
                HorusBackdrop()
                if model.accounts.isEmpty {
                    PairingView(canCancel: false)
                        .frame(maxWidth: 620)
                        .padding(24)
                } else {
                    shell
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
        #if os(iOS)
        .sensoryFeedback(.impact(weight: .light), trigger: model.toast?.id) { _, id in id != nil }
        #endif
        // Only a backgrounded scene loses its socket. `.inactive` covers a window losing focus
        // or a notification banner, and reconnecting on those drops a healthy session.
        .onChange(of: scenePhase) { _, newPhase in
            model.setSceneActive(newPhase != .background)
            if newPhase == .background {
                model.appDidEnterBackground()
            } else if newPhase == .active {
                Task { await model.appDidBecomeActive() }
            }
        }
        .task {
            model.start()
            if scenePhase == .active { await model.appDidBecomeActive() }
        }
    }

    /// Compact iOS reveals the sidebar under the detail; everything else keeps the split view,
    /// where two columns fit side by side and nothing has to slide out of the way.
    @ViewBuilder
    private var shell: some View {
        #if os(iOS)
        if horizontalSizeClass == .compact {
            SidebarDrawer(isOpen: $sidebarIsOpen) {
                SidebarView(showDetail: showDetail)
            } detail: {
                NavigationStack {
                    destination
                        .toolbar {
                            ToolbarItem(placement: .topBarLeading) {
                                Button {
                                    withAnimation(SidebarDrawerMetrics.animation) {
                                        sidebarIsOpen.toggle()
                                    }
                                } label: {
                                    HorusIcon(.menu, foreground: .primary)
                                }
                                .tint(.primary)
                                .accessibilityLabel(sidebarIsOpen ? "Hide sidebar" : "Show sidebar")
                            }
                        }
                }
            }
        } else {
            splitView
        }
        #else
        splitView
        #endif
    }

    private var splitView: some View {
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
        case .contribution(let id):
            if let widget = model.navigationWidgets.first(where: { $0.id == id }) {
                FrontendContributionPage(widget: widget)
            } else {
                HorusUnavailable(
                    title: "Capability unavailable",
                    glyph: .squaresFour,
                    detail: "This capability is not available in the current chat."
                )
            }
        }
    }

    private var preferredColorScheme: ColorScheme? {
        switch model.theme {
        case .system: nil
        case .dark: .dark
        case .light: .light
        }
    }

    /// Switches the page and brings it back on screen. The two belong in one transaction:
    /// setting the destination outside the animation swaps the page's content in a frame of its
    /// own, which reads as a jump before the slide rather than one move.
    private func showDetail(_ destination: AppDestination) {
        #if os(iOS)
        // The drawer keeps the detail mounted the whole time, so picking something in the
        // sidebar only has to slide it back over. The split view's compact column needed a
        // round trip through `.sidebar` here to re-fire a transition; nothing pushes now.
        if horizontalSizeClass == .compact {
            withAnimation(SidebarDrawerMetrics.animation) {
                model.destination = destination
                sidebarIsOpen = false
            }
            return
        }
        #endif
        model.destination = destination
        compactColumn = .detail
    }

    private var chatIsVisible: Bool {
        guard !model.accounts.isEmpty,
              model.destination == .chat,
              scenePhase == .active,
              !model.isAppLocked,
              !model.showsPairing,
              !model.showsWorkspaceBrowser
        else { return false }
        #if os(iOS)
        // The drawer, not the split view's column, decides whether the chat is on screen in
        // compact: `compactColumn` no longer moves there, so reading it would report the chat
        // permanently hidden and stop delivering it as visible.
        return horizontalSizeClass != .compact
            || !sidebarIsOpen && !model.showsInspector
        #else
        return true
        #endif
    }
}

private struct AppLockView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette

    var body: some View {
        ZStack {
            HorusBackdrop()
            HorusCard {
                VStack(spacing: 16) {
                    HorusIcon(
                        model.appLockAuthenticationMethod.glyph,
                        size: 36,
                        foreground: palette.accent
                    )
                    Text("Horus is locked")
                        .font(.title2.weight(.semibold))
                    Text(status)
                        .foregroundStyle(palette.muted)
                        .multilineTextAlignment(.center)
                        .accessibilityLabel("App lock status: \(status)")
                    if model.isAppLockAuthenticating {
                        ProgressView("Authenticating")
                    } else {
                        Button(
                            model.appLockError == nil
                                ? model.appLockAuthenticationMethod.unlockTitle
                                : "Try Again",
                            glyph: model.appLockError == nil ? .lockOpen : .arrowClockwise
                        ) {
                            Task { await model.unlockApp() }
                        }
                        .horusProminentButton()
                        .controlSize(.large)
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .frame(maxWidth: 380)
            .padding(24)
        }
    }

    private var status: String {
        model.appLockError ?? "Use Face ID or Touch ID to continue."
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
                    toast.tone.glyph,
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
                HorusIcon(.x, size: 14, foreground: palette.muted)
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.horusPlain)
            .accessibilityLabel("Dismiss notification")
        }
        .padding(.leading, 16)
        .padding(.trailing, 6)
        .padding(.vertical, 10)
        .horusGlass(in: HorusStyle.cardShape)
        .shadow(color: .black.opacity(0.20), radius: 18, y: 8)
        .gesture(
            DragGesture(minimumDistance: 20)
                .onEnded { value in
                    guard value.predictedEndTranslation.height < -40 else { return }
                    dismiss()
                }
        )
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

    var glyph: HorusGlyph {
        switch self {
        case .info: .info
        case .success: .checkCircle
        case .warning: .warning
        case .error: .xCircle
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
                                HorusLabel(title: entry.name, glyph: .folder)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.horusPlain)
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
                                glyph: .warning,
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
                    Button("Parent folder", glyph: .arrowUp) { onParent(parent) }
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
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        ArtifactView()
            .inspectorColumnWidth(min: 320, ideal: 520, max: 840)
            .quickLookPreview($model.previewURL)
            .sheet(item: $model.textFilePreview, onDismiss: model.discardAttachmentPreview) { preview in
                TextFilePreviewView(preview: preview)
            }
            .onChange(of: model.previewURL) { oldValue, newValue in
                if oldValue != nil, newValue == nil { model.discardAttachmentPreview() }
            }
    }
}

private struct FrontendContributionPage: View {
    @Environment(AppModel.self) private var model
    let widget: MountedWidget

    var body: some View {
        PageScaffold(title: widget.title, detail: detail) {
            if let content = widget.widget.content {
                Section {
                    FrontendWidgetContentView(content: content) { option in
                        model.submitPickerOption(option)
                    }
                }
            } else if widget.widget.action != nil {
                Section {
                    Button(
                        widget.widget.text,
                        glyph: widget.glyph,
                        action: { model.submitWidget(widget) }
                    )
                }
            } else {
                HorusUnavailable(
                    title: widget.widget.text,
                    glyph: widget.glyph,
                    detail: "No content is currently available."
                )
            }
        }
    }

    private var detail: String {
        if case .actionList? = widget.widget.content { return "" }
        return widget.widget.text == widget.title ? "" : widget.widget.text
    }
}

enum SidebarDrawerMetrics {
    static let width: CGFloat = 300
    /// How far in from the leading edge a closed drawer answers to a drag. The detail is full
    /// of scroll views that own horizontal drags of their own, so a closed drawer only takes
    /// the ones that start at the edge, the way the system back gesture does.
    static let edgeCatch: CGFloat = 24
    static let animation: Animation = .snappy(duration: 0.28)
    /// The display's corner radius, so the page reads as the phone rather than as a card.
    ///
    /// Concentric corners are the sanctioned way to ask for this, but they resolve against a
    /// container shape and nothing supplies one inside a mask — they come back square there.
    /// UIScreen knows the number and answers only to a private key, so this is the measured
    /// value for current iPhones instead; older, tighter displays round a touch generously.
    static let displayCornerRadius: CGFloat = 62
}

#if os(iOS)
/// Compact navigation that reveals the sidebar underneath instead of pushing a page over it.
///
/// The detail stays mounted and slides aside, so its scroll position, keyboard focus, and any
/// in-flight turn survive a trip to the sidebar and back — none of which a pushed page keeps.
private struct SidebarDrawer<Sidebar: View, Detail: View>: View {
    @Binding var isOpen: Bool
    @ViewBuilder let sidebar: Sidebar
    @ViewBuilder let detail: Detail

    @Environment(\.horusPalette) private var palette
    @GestureState private var drag: CGFloat = 0
    @State private var drawerFeedback = false

    var body: some View {
        ZStack(alignment: .leading) {
            // What the page's cut corners expose. The sidebar's own surface stops at its column,
            // which is exactly where the page's leading corners are, so without this the corners
            // reveal the app canvas — the same value the page carries, and the cut vanishes.
            palette.recessed.ignoresSafeArea()
            sidebar
                .frame(width: SidebarDrawerMetrics.width)
                .accessibilityHidden(!isOpen)
            detail
                // The sidebar sits directly behind, so the detail carries its own backdrop:
                // glass, which is what draws the lit rim along the edge the drawer exposes. It
                // is cut to `pageShape` so the page keeps the display's corners as it slides
                // aside — a plain rectangle fills those corners back in and the cut disappears.
                // Whatever sits over the glass has to stay clear of the rim, hence no scrim
                // tint: dimming the page washes the rim out and the edge stops reading as glass.
                .background {
                    // `ignoresSafeArea` first: glass renders into the frame it is given, so
                    // expanding it afterwards stretches a shape that was already cut to the
                    // inset rect and the corners come out square.
                    Color.clear
                        .ignoresSafeArea()
                        .glassEffect(.regular, in: pageShape)
                }
                .accessibilityHidden(isOpen)
                // Every page paints its own opaque backdrop, and the toolbar its own scroll
                // edge effect, both square and both over anything this view puts behind them.
                // Cutting the corners has to happen after all of it, on the way out.
                .mask { pageShape.ignoresSafeArea() }
                .overlay { pageEdge }
                .offset(x: offset)
                .simultaneousGesture(swipe)
        }
        .sensoryFeedback(.impact(weight: .light), trigger: drawerFeedback)
    }

    /// The display's shape, drawn in the display's own curve family rather than a plain rounded
    /// rectangle, so the page's corners sit on the bezel's.
    private var pageShape: ConcentricRectangle {
        ConcentricRectangle(corners: .fixed(SidebarDrawerMetrics.displayCornerRadius))
    }

    /// The lit rim of the page's glass, and the tap target that closes the drawer.
    ///
    /// Glass over the sidebar's flat canvas has nothing to refract, so the material alone barely
    /// registers; the specular edge is the part that reads. Only the leading edge gets one —
    /// stroking the whole shape runs a line along the top and bottom of the display, where it
    /// meets the bezel's own curve and the two roundings visibly disagree. Fading the ends off
    /// keeps the highlight clear of the corners entirely.
    @ViewBuilder
    private var pageEdge: some View {
        if progress > 0 {
            pageShape
                .stroke(
                    LinearGradient(
                        // The Nord swatches themselves are file-private to HorusStyle; the
                        // palette's accent is that same frost blue and is what every other view
                        // reaches for, so the tint follows it instead of a raw nord constant.
                        stops: [
                            .init(color: palette.accent.opacity(0.32), location: 0),
                            .init(color: palette.accent.opacity(0.16), location: 0.06),
                            .init(color: .clear, location: 0.22)
                        ],
                        startPoint: .leading,
                        endPoint: .trailing
                    )
                    .opacity(progress),
                    lineWidth: 1
                )
                .ignoresSafeArea()
                // Light falls off down the edge the way it does on a real bevel; an even line
                // reads as a drawn border instead of a lit one.
                .mask {
                    LinearGradient(
                        stops: [
                            .init(color: .white, location: 0),
                            .init(color: .white.opacity(0.45), location: 0.45),
                            .init(color: .white.opacity(0.25), location: 1)
                        ],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                    .ignoresSafeArea()
                }
                .contentShape(Rectangle())
                .onTapGesture { setOpen(false) }
                .accessibilityElement()
                .accessibilityLabel("Close sidebar")
                .accessibilityAddTraits(.isButton)
                .accessibilityAction { setOpen(false) }
        }
    }

    private var offset: CGFloat {
        min(max((isOpen ? SidebarDrawerMetrics.width : 0) + drag, 0), SidebarDrawerMetrics.width)
    }

    private var progress: Double { Double(offset / SidebarDrawerMetrics.width) }

    private var swipe: some Gesture {
        DragGesture(minimumDistance: 12)
            .updating($drag) { value, state, _ in
                guard accepts(value) else { return }
                state = value.translation.width
            }
            .onEnded { value in
                guard accepts(value) else { return }
                let projected = (isOpen ? SidebarDrawerMetrics.width : 0)
                    + value.predictedEndTranslation.width
                setOpen(projected > SidebarDrawerMetrics.width / 2)
            }
    }

    private func accepts(_ value: DragGesture.Value) -> Bool {
        guard abs(value.translation.width) > abs(value.translation.height) else { return false }
        return isOpen || value.startLocation.x <= SidebarDrawerMetrics.edgeCatch
    }

    private func setOpen(_ open: Bool) {
        guard isOpen != open else { return }
        drawerFeedback.toggle()
        withAnimation(SidebarDrawerMetrics.animation) { isOpen = open }
    }
}
#endif

struct SidebarView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.horusPalette) private var palette
    let showDetail: (AppDestination) -> Void
    @State private var collapsedWorkspaces: Set<String> = []
    @State private var sessionToRename: SessionRecord?
    @State private var renameDraft = ""
    @State private var sessionToDelete: SessionRecord?
    @State private var showsConnectionDetails = false

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                HStack(spacing: 10) {
                    Image("HorusLogo")
                        .resizable()
                        .scaledToFit()
                        .frame(width: 28, height: 28)
                        .clipShape(.rect(cornerRadius: 6))
                        .accessibilityHidden(true)
                    Text("HORUS")
                        .font(.system(.subheadline, design: .serif, weight: .bold))
                        .tracking(1.4)
                    Spacer()
                    Button {
                        showsConnectionDetails = true
                    } label: {
                        // A solid dot, not an icon: HugeIcons is a stroked set with no
                        // filled circle, and an outlined ring reads as a control here.
                        Circle()
                            .fill(model.connectionState.isReady ? palette.signal : palette.danger)
                            .frame(width: 8, height: 8)
                            .symbolEffect(
                                .pulse.byLayer,
                                options: .repeat(.continuous),
                                isActive: !reduceMotion
                            )
                            .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.horusPlain)
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
                    ForEach(model.navigationWidgets) { widget in
                        contributionNavigationButton(widget)
                    }
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
        // The split view paints its own system background over the app backdrop, and in compact
        // the page slides over this, so it sits a step under the canvas rather than matching it.
        .background { palette.recessed.ignoresSafeArea() }
        .safeAreaInset(edge: .bottom) {
            HStack {
                Button {
                    model.openNewSession()
                    showDetail(.chat)
                } label: {
                    HorusLabel(title: "New chat", glyph: .notePencil)
                        .font(HorusStyle.controlFont)
                }
                .horusProminentButton()
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .disabled(!model.canCreateSession)
                .help("New chat")
                Spacer()
                Button {
                    showDetail(.profile)
                } label: {
                    HorusIcon(.gear)
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
                    HorusLabel(title: "Retry connection", glyph: .arrowClockwise)
                }
                .disabled(model.selectedAccount == nil)
                Button {
                    showsConnectionDetails = false
                    model.repairSelectedGateway()
                } label: {
                    HorusLabel(title: "Repair pairing", glyph: .link)
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
            showDetail(destination)
        } label: {
            HorusLabel(
                title: title,
                glyph: destination.glyph,
                iconColor: model.destination == destination ? palette.accent : Color.primary
            )
                .font(HorusStyle.controlFont)
                .foregroundStyle(model.destination == destination ? palette.accent : Color.primary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .padding(.horizontal, 4)
        .frame(minHeight: HorusStyle.iconButtonSize)
    }

    private func contributionNavigationButton(_ widget: MountedWidget) -> some View {
        let destination = AppDestination.contribution(widget.id)
        return Button {
            if widget.widget.action != nil {
                model.submitWidget(widget)
            }
            showDetail(destination)
        } label: {
            HorusLabel(
                title: widget.widget.text,
                glyph: widget.glyph,
                iconColor: model.destination == destination ? palette.accent : Color.primary
            )
            .font(HorusStyle.controlFont)
            .foregroundStyle(model.destination == destination ? palette.accent : Color.primary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
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
                        HorusIcon(.folder, foreground: palette.muted)
                        Text(group.name)
                            .font(HorusStyle.controlFont)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        HorusIcon(
                            collapsedWorkspaces.contains(group.id)
                                ? .caretRight
                                : .caretDown,
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
                .buttonStyle(.horusPlain)
                .accessibilityValue(
                    collapsedWorkspaces.contains(group.id) ? "Collapsed" : "Expanded"
                )
                .help(group.path)

                Button {
                    model.chooseWorkspace(group.path)
                    showDetail(.chat)
                } label: {
                    HorusLabel(
                        title: "New chat in \(group.name)",
                        glyph: .notePencil,
                        iconColor: palette.muted,
                        iconSize: 14
                    )
                    .labelStyle(.iconOnly)
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.horusPlain)
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
                showDetail(.chat)
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
            .buttonStyle(.horusPlain)
            .disabled(!model.canOpenSession && session.sessionId != model.selectedSessionID)
            .accessibilityValue(activityValue)
            .accessibilityAddTraits(isSelected ? .isSelected : [])

            if session.pinned {
                HorusIcon(.pushPin, size: 12, foreground: palette.accent)
            }

            Menu {
                Button {
                    model.setSessionPinned(session, pinned: !session.pinned)
                } label: {
                    HorusPlatformMenuLabel(
                        title: session.pinned ? "Unpin" : "Pin",
                        glyph: session.pinned ? .pushPinSlash : .pushPin,
                        systemImage: session.pinned ? "pin.slash" : "pin"
                    )
                }
                Button {
                    renameDraft = session.displayTitle
                    sessionToRename = session
                } label: {
                    HorusPlatformMenuLabel(
                        title: "Rename",
                        glyph: .pencilSimple,
                        systemImage: "pencil"
                    )
                }
                Divider()
                Button(role: .destructive) {
                    sessionToDelete = session
                } label: {
                    HorusPlatformMenuLabel(
                        title: "Delete",
                        glyph: .trash,
                        systemImage: "trash"
                    )
                }
            } label: {
                #if os(macOS)
                Image(systemName: "ellipsis")
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
                #else
                HorusIcon(.dotsThree)
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
                #endif
            }
            .labelStyle(.titleAndIcon)
            .buttonStyle(.horusPlain)
            .tint(.primary)
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
                        Button("Close", glyph: .x) {
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
                            glyph: .warning,
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
        #if os(iOS)
        .scrollDismissesKeyboard(.interactively)
        #endif
        .safeAreaInset(edge: .bottom) { pairAction }
        .onSubmit { model.pair() }
    }

    private var pairAction: some View {
        VStack(spacing: 14) {
            HorusLabel(
                title: "4-byte framed JSON · protocol v\(gatewayProtocolVersion)",
                glyph: .shieldCheck,
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
