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
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @State private var columnVisibility = NavigationSplitViewVisibility.all
    @State private var compactColumn = debugStartsOnDetail ? NavigationSplitViewColumn.detail : .sidebar
    @State private var sidebarIsOpen = !debugStartsOnDetail

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
                        .padding(HorusSpace.xl)
                } else {
                    shell
                        .inspector(isPresented: $model.showsInspector) {
                            FilesInspector()
                                .overlay(alignment: .top) {
                                    if horizontalSizeClass == .compact { AppToastOverlay() }
                                }
                        }
                        .sheet(isPresented: $model.showsPairing) {
                            PairingView(canCancel: true)
                                .frame(maxWidth: 560)
                                .padding(HorusSpace.xl)
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
        .alert(
            "Rename chat",
            isPresented: Binding(
                get: { model.sessionToRename != nil },
                set: { if !$0 { model.sessionToRename = nil } }
            )
        ) {
            TextField("Chat name", text: $model.sessionRenameDraft)
            Button("Cancel", role: .cancel) { model.sessionToRename = nil }
            Button("Rename") {
                guard let session = model.sessionToRename,
                      model.renameSession(session, title: model.sessionRenameDraft) != nil
                else { return }
                model.sessionToRename = nil
            }
            .disabled(
                model.sessionRenameDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    || !model.canRenameSession
            )
        }
        .confirmationDialog(
            "Delete this chat?",
            isPresented: Binding(
                get: { model.sessionToDelete != nil },
                set: { if !$0 { model.sessionToDelete = nil } }
            ),
            titleVisibility: .visible
        ) {
            Button("Delete chat", role: .destructive) {
                if let session = model.sessionToDelete { model.deleteSession(session) }
                model.sessionToDelete = nil
            }
            .disabled(!model.canRenameSession)
            Button("Cancel", role: .cancel) { model.sessionToDelete = nil }
        } message: {
            Text("This removes the chat from the gateway history.")
        }
        .quickLookPreview($model.previewURL)
        .sheet(item: $model.textFilePreview, onDismiss: model.discardFilePresentation) { preview in
            TextFilePreviewView(preview: preview)
        }
        .sheet(item: $model.sessionFileShareItem, onDismiss: model.discardFilePresentation) { file in
            SessionFileShareView(file: file)
        }
        .onChange(of: model.previewURL) { oldValue, newValue in
            if oldValue != nil, newValue == nil { model.discardFilePresentation() }
        }
        .preferredColorScheme(preferredColorScheme)
        .onChange(of: chatIsVisible, initial: true) { _, visible in
            model.setChatVisible(visible)
        }
        .onChange(of: model.chatRoute) { _, route in
            guard route != nil, horizontalSizeClass == .compact else { return }
            withAnimation(SidebarDrawerMetrics.animation) { sidebarIsOpen = false }
        }
        .onChange(of: model.toast?.id) { _, _ in
            guard let toast = model.toast else { return }
            AccessibilityNotification.Announcement(
                "\(toast.tone.title): \(toast.message)"
            ).post()
        }
        .sensoryFeedback(.impact(weight: .light), trigger: model.toast?.id) { _, id in id != nil }
        .sensoryFeedback(.impact(weight: .light), trigger: model.steeringDeliveryRevision)
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
        if horizontalSizeClass == .compact {
            SidebarDrawer(isOpen: $sidebarIsOpen) {
                SidebarView(showDetail: showDetail)
            } detail: {
                detailNavigation
            }
        } else {
            splitView
        }
    }

    private var splitView: some View {
        NavigationSplitView(
            columnVisibility: $columnVisibility,
            preferredCompactColumn: $compactColumn
        ) {
            SidebarView(showDetail: showDetail)
                .navigationSplitViewColumnWidth(min: 230, ideal: 272, max: 340)
        } detail: {
            detailNavigation
        }
        .navigationSplitViewStyle(.balanced)
    }

    private var detailNavigation: some View {
        @Bindable var model = model
        return NavigationStack(path: $model.chatNavigationPath) {
            destination
                .navigationDestination(for: ChatRoute.self) { route in
                    switch route {
                    case .session: ChatView()
                    }
                }
                .toolbar {
                    if horizontalSizeClass == .compact && model.chatNavigationPath.isEmpty {
                        ToolbarItem(placement: .topBarLeading) {
                            Button {
                                // The keyboard belongs to the page being slid away; left
                                // up, it animates against a screen the reader just left.
                                model.dismissComposerFocus()
                                withAnimation(SidebarDrawerMetrics.animation) {
                                    sidebarIsOpen.toggle()
                                }
                            } label: {
                                // A bare glyph is a 16pt target. Every other icon button
                                // in the app pads itself out to a full one, and without
                                // that this took two or three tries to hit.
                                HorusIcon(.menu, foreground: .primary)
                                    .frame(
                                        width: HorusStyle.iconButtonSize,
                                        height: HorusStyle.iconButtonSize
                                    )
                                    .contentShape(Rectangle())
                            }
                            .tint(.primary)
                            .accessibilityLabel(sidebarIsOpen ? "Hide sidebar" : "Show sidebar")
                        }
                    }
                }
        }
    }

    @ViewBuilder
    private var destination: some View {
        switch model.destination ?? .chats {
        case .chats: ChatsView()
        case .gateway: GatewayView()
        case .agent: AgentSettingsView(scope: .gatewayDefault)
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
        // The drawer keeps the detail mounted the whole time, so picking something in the
        // sidebar only has to slide it back over. The split view's compact column needed a
        // round trip through `.sidebar` here to re-fire a transition; nothing pushes now.
        if horizontalSizeClass == .compact {
            withAnimation(SidebarDrawerMetrics.animation) {
                model.chatRoute = nil
                model.destination = destination
                sidebarIsOpen = false
            }
            return
        }
        model.chatRoute = nil
        model.destination = destination
        compactColumn = .detail
    }

    private var chatIsVisible: Bool {
        guard !model.accounts.isEmpty,
              model.destination == .chats,
              !model.chatNavigationPath.isEmpty,
              scenePhase == .active,
              !model.isAppLocked,
              !model.showsPairing,
              !model.showsWorkspaceBrowser,
              !model.showsInspector
        else { return false }
        // The drawer, not the split view's column, decides whether the chat is on screen in
        // compact: `compactColumn` no longer moves there, so reading it would report the chat
        // permanently hidden and stop delivering it as visible.
        return horizontalSizeClass != .compact || !sidebarIsOpen
    }
}

private struct AppLockView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @ScaledMetric(relativeTo: .largeTitle) private var iconSize: CGFloat = 72

    var body: some View {
        ZStack {
            HorusBackdrop()
            Button {
                Task { await model.unlockApp() }
            } label: {
                HorusIcon(
                    model.appLockError == nil
                        ? model.appLockAuthenticationMethod.glyph
                        : .warningOctagon,
                    size: iconSize,
                    foreground: model.appLockError == nil ? palette.accent : palette.danger
                )
                .frame(width: 128, height: 128)
                .contentShape(Circle())
            }
            .buttonStyle(.horusPlain)
            .disabled(model.isAppLockAuthenticating)
            .opacity(model.isAppLockAuthenticating ? 0.45 : 1)
            .accessibilityLabel(
                model.appLockError == nil
                    ? model.appLockAuthenticationMethod.unlockTitle
                    : "Try Again"
            )
            .accessibilityValue(
                model.isAppLockAuthenticating
                    ? "Authenticating"
                    : model.appLockError ?? "Horus is locked"
            )
        }
    }
}

private struct AppToastOverlay: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ZStack {
            if let toast = model.toast {
                AppToastView(toast: toast, dismiss: dismiss)
                    .transition(
                        reduceMotion
                            ? .opacity
                            : .move(edge: .top).combined(with: .opacity)
                    )
            }
        }
        .frame(maxWidth: 520)
        .padding(.horizontal, HorusSpace.l)
        .padding(.top, HorusSpace.m)
        .allowsHitTesting(model.toast != nil)
        .animation(toastAnimation, value: model.toast?.id)
    }

    private var toastAnimation: Animation {
        reduceMotion ? .easeOut(duration: 0.12) : .smooth(duration: 0.28)
    }

    private func dismiss() {
        withAnimation(toastAnimation) { model.dismissToast() }
    }
}

private struct AppToastView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let toast: AppToast
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: HorusSpace.m) {
            if let sessionID = toast.sessionID {
                Button {
                    model.showsInspector = false
                    model.showsPairing = false
                    model.showsWorkspaceBrowser = false
                    model.openChat(sessionID)
                    dismiss()
                } label: {
                    toastMessage
                }
                .buttonStyle(.horusPlain)
                .accessibilityLabel(accessibilityLabel)
                .accessibilityHint("Opens this chat")
            } else {
                toastMessage
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(accessibilityLabel)
            }

            Button(action: dismiss) {
                HorusIcon(.x, size: HorusStyle.glyphInline, foreground: palette.muted)
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.horusPlain)
            .accessibilityLabel("Dismiss notification")
        }
        .padding(.leading, HorusSpace.l)
        .padding(.trailing, HorusSpace.s)
        .padding(.vertical, HorusSpace.m)
        .horusGlass(in: HorusStyle.cardShape, interactive: true)
        .shadow(color: .black.opacity(0.20), radius: 18, y: 8)
        .gesture(
            DragGesture(minimumDistance: 20)
                .onEnded { value in
                    guard value.predictedEndTranslation.height < -40 else { return }
                    dismiss()
                }
        )
    }

    private var toastMessage: some View {
        HStack(alignment: .top, spacing: HorusSpace.m) {
            HorusIcon(
                toast.tone.glyph,
                size: 18,
                foreground: toast.tone.color(in: palette)
            )
            VStack(alignment: .leading, spacing: HorusSpace.xxs) {
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
        .contentShape(Rectangle())
    }

    private var accessibilityLabel: String {
        "\(toast.tone.title): \(toast.message)"
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
    @State private var newFolderName = ""
    @State private var showsNewFolderPrompt = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if let listing = model.directoryListing {
                    DirectoryBrowserHeader(
                        path: listing.path,
                        title: "Choose a workspace for the new chat",
                        parent: listing.parent,
                        onParent: model.loadDirectory,
                        onCreateFolder: {
                            newFolderName = ""
                            showsNewFolderPrompt = true
                        }
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
            .disabled(model.isLoadingDirectories || model.isChangingWorkspace)
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
                            || model.isLoadingDirectories
                            || model.isChangingWorkspace
                    )
                }
            }
        }
        .alert("New folder", isPresented: $showsNewFolderPrompt) {
            TextField("Folder name", text: $newFolderName)
            Button("Cancel", role: .cancel) {}
            Button("Create") {
                model.createWorkspaceDirectory(named: newFolderName)
            }
            .disabled(newFolderName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        } message: {
            Text("Create a folder inside \(model.directoryListing?.path ?? "this location").")
        }
    }
}

private struct DirectoryBrowserHeader: View {
    @Environment(\.horusPalette) private var palette
    let path: String
    let title: String
    let parent: String?
    let onParent: (String) -> Void
    let onCreateFolder: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: HorusSpace.xs) {
            Text(path)
                .font(HorusStyle.metadataFont.weight(.bold))
                .tracking(1)
                .foregroundStyle(palette.accent)
                .lineLimit(2)
            HStack {
                Text(title)
                    .font(HorusStyle.controlFont)
                Spacer()
                Button("New folder", glyph: .folderPlus, action: onCreateFolder)
                    .labelStyle(.iconOnly)
                    .buttonStyle(HorusIconButtonStyle())
                    .help("New folder")
                if let parent {
                    Button("Parent folder", glyph: .arrowUp) { onParent(parent) }
                        .labelStyle(.iconOnly)
                        .buttonStyle(HorusIconButtonStyle())
                        .help("Parent folder")
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, HorusSpace.l)
        .padding(.vertical, HorusSpace.s)
    }
}

private struct FilesInspector: View {
    var body: some View {
        FilesView()
            .inspectorColumnWidth(min: 320, ideal: 520, max: 840)
    }
}

private struct FrontendContributionPage: View {
    @Environment(AppModel.self) private var model
    let widget: MountedWidget

    var body: some View {
        PageScaffold(title: widget.title, detail: detail) {
            if !model.isCapabilityEnabled(widget.capability) {
                DisabledCapabilityNotice(
                    title: "\(widget.widget.text) is off",
                    detail: "Saved content remains visible. Enable \(widget.widget.text) in this chat to make changes."
                )
            }
            if let content = widget.widget.content {
                Section {
                    FrontendWidgetContentView(
                        content: content,
                        actionsEnabled: model.isCapabilityEnabled(widget.capability)
                    ) { option in
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
    /// How strongly the page is tinted once the drawer is fully open.
    static let scrimOpacity: Double = 0.45
    /// How far behind the page the sidebar starts before it comes forward.
    static let sidebarDepth: CGFloat = 0.08
}

/// Compact navigation that reveals the sidebar underneath instead of pushing a page over it.
///
/// The detail stays mounted and slides aside, so its scroll position, keyboard focus, and any
/// in-flight turn survive a trip to the sidebar and back — none of which a pushed page keeps.
private struct SidebarDrawer<Sidebar: View, Detail: View>: View {
    @Binding var isOpen: Bool
    @ViewBuilder let sidebar: Sidebar
    @ViewBuilder let detail: Detail

    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @State private var drag: CGFloat = 0
    @State private var drawerFeedback = false

    var body: some View {
        ZStack(alignment: .leading) {
            // What the page's cut corners expose. The sidebar's own surface stops at its column,
            // which is exactly where the page's leading corners are, so without this the corners
            // reveal the app canvas — the same value the page carries, and the cut vanishes.
            palette.recessed.ignoresSafeArea()
            sidebar
                .frame(width: SidebarDrawerMetrics.width)
                // The sidebar comes forward from behind the page rather than waiting in place
                // for it to move. Depth is what says the page is on top, and a transform
                // costs nothing to animate.
                .scaleEffect(
                    1 - SidebarDrawerMetrics.sidebarDepth * (1 - progress),
                    anchor: .leading
                )
                .opacity(0.4 + 0.6 * progress)
                .accessibilityHidden(!isOpen)
            detail
                .accessibilityHidden(isOpen)
                // Scrim first, mask second: one pass cuts the page and the dimming over it to
                // the same corners rather than each paying for its own. Every page paints its
                // own opaque backdrop and the toolbar its own scroll edge effect, both square,
                // so cutting the corners has to happen after all of it, on the way out.
                .overlay { scrim }
                .mask { pageShape.ignoresSafeArea() }
                .offset(x: offset)
                .scrollDisabled(drag != 0)
                .simultaneousGesture(swipe)
        }
        .sensoryFeedback(.impact(weight: .light), trigger: drawerFeedback)
    }

    /// The display's shape, drawn in the display's own curve family rather than a plain rounded
    /// rectangle, so the page's corners sit on the bezel's.
    private var pageShape: ConcentricRectangle {
        ConcentricRectangle(corners: .fixed(SidebarDrawerMetrics.displayCornerRadius))
    }

    /// The page is tinted as it slides, which separates it from the sidebar behind, and
    /// is the tap target that closes the drawer.
    ///
    /// This replaces a lit glass rim along the leading edge. That rim existed only because
    /// nothing marked the boundary — glass over the sidebar's flat canvas barely registers,
    /// so the specular edge was doing all the work, and a scrim would have washed it out.
    /// Tinting the page states the same thing directly, and costs a colour instead of a
    /// real-time material, a stroked gradient and a second mask on every frame of the slide.
    private var scrim: some View {
        palette.sidebarScrim
            .opacity(SidebarDrawerMetrics.scrimOpacity * progress)
            .ignoresSafeArea()
            .allowsHitTesting(progress > 0)
            .onTapGesture { setOpen(false) }
            .accessibilityHidden(progress == 0)
            .accessibilityLabel("Close sidebar")
            .accessibilityAddTraits(.isButton)
            .accessibilityAction { setOpen(false) }
    }

    private var offset: CGFloat {
        min(max((isOpen ? SidebarDrawerMetrics.width : 0) + drag, 0), SidebarDrawerMetrics.width)
    }

    private var progress: Double { Double(offset / SidebarDrawerMetrics.width) }

    /// The drag is plain state, not `@GestureState`, and is cleared inside the same animated
    /// transaction that settles the drawer.
    ///
    /// `@GestureState` resets itself the moment the gesture ends, and that reset lands
    /// outside any animation: the page snapped back to where it started, then animated open
    /// from there. Releasing a pull looked like the drawer opening twice.
    private var swipe: some Gesture {
        DragGesture(minimumDistance: 12)
            .onChanged { value in
                guard accepts(value) else { return }
                if !isOpen, drag == 0, value.translation.width > 0 {
                    model.dismissComposerFocus()
                }
                drag = value.translation.width
            }
            .onEnded { value in
                guard accepts(value) else {
                    drag = 0
                    return
                }
                let projected = (isOpen ? SidebarDrawerMetrics.width : 0)
                    + value.predictedEndTranslation.width
                let open = projected > SidebarDrawerMetrics.width / 2
                if open != isOpen { drawerFeedback.toggle() }
                withAnimation(SidebarDrawerMetrics.animation) {
                    drag = 0
                    isOpen = open
                }
            }
    }

    private func accepts(_ value: DragGesture.Value) -> Bool {
        guard abs(value.translation.width) > abs(value.translation.height) else { return false }
        if !isOpen, !model.chatNavigationPath.isEmpty { return false }
        return isOpen || value.startLocation.x <= SidebarDrawerMetrics.edgeCatch
    }

    private func setOpen(_ open: Bool) {
        guard isOpen != open else { return }
        drawerFeedback.toggle()
        // Clears any drag a cancelled gesture left behind, which no longer resets itself.
        withAnimation(SidebarDrawerMetrics.animation) {
            drag = 0
            isOpen = open
        }
    }
}

struct SidebarView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.horusPalette) private var palette
    let showDetail: (AppDestination) -> Void
    @State private var showsConnectionDetails = false

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                HStack(spacing: HorusSpace.m) {
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
                .padding(.horizontal, HorusSpace.l)
                .padding(.vertical, HorusSpace.m)

                VStack(alignment: .leading, spacing: HorusSpace.xxs) {
                    navigationButton("Chats", destination: .chats)
                    navigationButton("Gateway", destination: .gateway)
                    navigationButton("Providers", destination: .providers)
                    navigationButton("Default agent", destination: .agent)
                    navigationButton("Cron", destination: .cron)
                    ForEach(model.navigationWidgets) { widget in
                        contributionNavigationButton(widget)
                    }
                }
                .padding(.horizontal, HorusSpace.m)
                .padding(.bottom, HorusSpace.m)
            }
            .frame(maxWidth: .infinity)
        }
        .font(HorusStyle.bodyFont)
        // The split view paints its own system background over the app backdrop, and in compact
        // the page slides over this, so it sits a step under the canvas rather than matching it.
        .background { palette.recessed.ignoresSafeArea() }
        .safeAreaInset(edge: .bottom) {
            settingsButton
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, HorusSpace.m)
                .padding(.vertical, HorusSpace.s)
        }
        .toolbarVisibility(.hidden, for: .navigationBar)
    }

    private var settingsButton: some View {
        Button("Settings", glyph: AppDestination.profile.glyph) {
            showDetail(.profile)
        }
        .labelStyle(.iconOnly)
        .buttonStyle(HorusIconButtonStyle(prominent: model.destination == .profile))
        .help("Settings")
    }

    private var connectionDetails: some View {
        VStack(spacing: HorusSpace.m) {
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
        .padding(HorusSpace.l)
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
        .padding(.horizontal, HorusSpace.xs)
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
        .padding(.horizontal, HorusSpace.xs)
        .frame(minHeight: HorusStyle.iconButtonSize)
    }

}

struct PairingView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.horusPalette) private var palette
    let canCancel: Bool

    var body: some View {
        @Bindable var model = model
        ScrollView {
            VStack(alignment: .leading, spacing: HorusSpace.xl) {
                HStack(alignment: .top) {
                    SectionHeading(
                        title: "Pair with a gateway",
                        detail: "Use the same address and one-time code on iPad or iPhone."
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

                VStack(spacing: HorusSpace.m) {
                    HorusCard {
                        VStack(alignment: .leading, spacing: HorusSpace.l) {
                            VStack(alignment: .leading, spacing: HorusSpace.s) {
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
                                    .labelStyle(.iconOnly)
                                    .buttonStyle(.glass)
                                    .buttonBorderShape(.circle)
                                    .controlSize(.large)
                                    .frame(
                                        width: HorusStyle.iconButtonSize,
                                        height: HorusStyle.iconButtonSize
                                    )
                                    .accessibilityLabel("Paste pairing setup")
                                    .help("Paste pairing setup")
                                }
                            }
                            VStack(alignment: .leading, spacing: HorusSpace.s) {
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
                        .padding(.horizontal, HorusSpace.s)

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
        .scrollDismissesKeyboard(.interactively)
        .safeAreaInset(edge: .bottom) { pairAction }
        .onSubmit { model.pair() }
    }

    /// The two ways in, then the wire detail. The protocol line led this stack before, which
    /// put the most technical line on the screen above the decision it belongs under.
    private var pairAction: some View {
        VStack(spacing: HorusSpace.m) {
            if model.connectionState == .connecting || model.connectionState == .authenticating {
                HStack {
                    HorusSpinner(size: HorusStyle.glyphLead, foreground: palette.accent)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(
                    model.connectionState == .authenticating
                        ? Text("Authenticating with gateway")
                        : Text("Connecting to gateway")
                )
            }
            Button("Pair to self-hosted gateway", action: model.pair)
                .horusProminentButton()
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .buttonSizing(.flexible)
            HorusCloudOfferButton()
            HorusLabel(
                title: "4-byte framed JSON · protocol v\(gatewayProtocolVersion)",
                glyph: .shieldCheck,
                iconColor: palette.muted
            )
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
                .padding(.top, HorusSpace.xxs)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, HorusSpace.l)
    }
}

extension String {
    var nonEmpty: String? {
        let trimmed = trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}
