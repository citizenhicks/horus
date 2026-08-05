import Foundation
import Observation

enum AppDestination: Equatable {
    case chat
    case gateway
    case agent
    case providers
    case cron
    case profile

    var systemImage: String {
        switch self {
        case .chat: "bubble.left.and.bubble.right"
        case .gateway: "server.rack"
        case .agent: "slider.horizontal.3"
        case .providers: "network"
        case .cron: "calendar.badge.clock"
        case .profile: "gearshape"
        }
    }
}

enum ConnectionState: Equatable {
    case disconnected
    case connecting
    case authenticating
    case loading
    case ready
    case failed(String)

    var label: String {
        switch self {
        case .disconnected: "Offline"
        case .connecting: "Connecting"
        case .authenticating: "Authenticating"
        case .loading: "Opening workspace"
        case .ready: "Ready"
        case .failed: "Needs attention"
        }
    }

    var isReady: Bool { self == .ready }
}

enum ApplyState: Equatable {
    case idle
    case applying
    case restarting
    case applied
    case busy(String)
    case conflict(String)
    case invalid(String)
    case failed(String)
}

enum ProviderActionState: Equatable {
    case idle
    case savingCredential(String)
    case credentialSaved(String)
    case startingLogin(String)
    case deviceCode(provider: String, url: String, code: String)
    case loginFinished(String)
    case failed(String)
}

enum ToastTone: Equatable {
    case info
    case success
    case warning
    case error
}

struct AppToast: Identifiable {
    let id = UUID()
    let message: String
    let tone: ToastTone
}

enum ThemePreference: String, CaseIterable, Identifiable {
    case system
    case dark
    case light

    var id: Self { self }
}

@Observable
final class TranscriptEntry: Identifiable {
    enum Kind: Equatable {
        case user
        case assistant
        case reasoning
        case event
        case error
    }

    let id: String
    var text: String
    var kind: Kind
    var group: String?
    var format: String
    var tone: String
    var pending: Bool

    init(
        id: String,
        text: String,
        kind: Kind,
        group: String? = nil,
        format: String,
        tone: String = "neutral",
        pending: Bool
    ) {
        self.id = id
        self.text = text
        self.kind = kind
        self.group = group
        self.format = format
        self.tone = tone
        self.pending = pending
    }
}

struct ApprovalCall: Identifiable, Equatable {
    let id: String
    let name: String
    let arguments: String
}

struct PendingApproval: Equatable {
    let id: String
    let reason: String
    let calls: [ApprovalCall]
}

struct PairingCodeInfo: Equatable {
    let code: String
    let expiresAt: Date
}

struct MountedWidget: Identifiable, Sendable {
    let capability: String
    let widget: FrontendWidget

    var id: String { "\(capability)\u{0}\(widget.id)" }
}

struct MountedCommand: Identifiable, Sendable {
    let capability: String
    let command: FrontendCommand

    var id: String { "\(capability)\u{0}\(command.name)" }
}

struct MountedReference: Identifiable, Sendable {
    let capability: String
    let reference: FrontendReference

    var id: String { "\(capability)\u{0}\(reference.trigger)\u{0}\(reference.value)" }
    var replacement: String { "\(reference.trigger)\(reference.value)" }
}

struct MiddlewareContributionCount: Identifiable, Equatable, Sendable {
    let id: String
    let label: String
    let value: Int
}

private enum ConfigurationTarget {
    case session
    case defaultAgent
}

struct ReferenceSuggestions {
    let range: Range<String.Index>
    let matches: [MountedReference]
}

struct PreviewBlock: Identifiable, Sendable {
    let id: String
    let block: FrontendBlock
}

struct TranscriptPreview: Identifiable, Sendable {
    let id: String
    let title: String
    let status: String?
    let model: String?
    let blocks: [PreviewBlock]
}

struct FrontendPickerPrompt: Sendable {
    let title: String
    let options: [FrontendPickerOption]
}

@MainActor
@Observable
final class AppModel {
    var accounts: [GatewayAccount]
    var selectedAccountID: UUID?
    var connectionState: ConnectionState = .disconnected
    var destination: AppDestination? = .chat
    var workspace: WorkspaceInfo?
    var gitStatus: GitStatus?
    var gitDiff = ""
    var sessions: [SessionRecord] = []
    var selectedSessionID: String?
    private(set) var unreadSessionIDs: Set<String> = []
    var transcript: [TranscriptEntry] = []
    var composer = ""
    var toast: AppToast?
    var activeTurnID: String?
    var activeOperation: String?
    var steeringQueued = false
    var contextTokens = 0
    var modelContextWindow: Int64?
    var pendingApproval: PendingApproval?
    var modelChoices: [ModelChoice] = []
    var middlewareFeatures: [MiddlewareFeature] = []
    var selectedModelRoute = ""
    var contributions: [FrontendContribution] = []
    var toolCount = 0
    var mountedWidgets: [MountedWidget] = []
    var pendingPicker: FrontendPickerPrompt?
    var artifacts: [ArtifactRecord] = []
    var previews: [TranscriptPreview] = []
    var presentedPreview: TranscriptPreview?
    var selectedArtifactID: String?
    var showsInspector = false
    var profile: ProfileSnapshot?
    var currentUsage = TokenUsage()
    var lastUsage = TokenUsage()
    var cronTasks: [CronTask] = []
    var cronRuns: [CronRun] = []
    var cronTaskDraft = ""
    var cronError: String?
    var workspaceError: String?
    var isChangingWorkspace = false
    var showsWorkspaceBrowser = false
    var directoryListing: DirectoryListing?
    var directoryError: String?
    var isLoadingDirectories = false

    var agentSnapshot: VersionedAgentConfig?
    var defaultAgentSnapshot: VersionedAgentConfig?
    var agentDraft: AgentComposition?
    var applyState: ApplyState = .idle
    var providerStatuses: [ProviderStatus] = []
    var providerAPIKey = ""
    var providerActionState: ProviderActionState = .idle
    var pairingCodeInfo: PairingCodeInfo?

    var showsPairing = false
    var pairingEndpoint = "wss://"
    var pairingCode = ""
    var pairingError: String?
    var theme: ThemePreference

    @ObservationIgnored private let client: GatewayClient
    @ObservationIgnored private let store: GatewayStore
    @ObservationIgnored private let requestSender:
        @MainActor @Sendable (GatewayRequest) async throws -> Void
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    @ObservationIgnored private var connectionGeneration = UUID()
    @ObservationIgnored private var pendingPairingAccount: GatewayAccount?
    @ObservationIgnored private var pendingDrafts: [String: String] = [:]
    @ObservationIgnored private var sessionRequestID: String?
    private var sessionMutationRequestID: String?
    @ObservationIgnored private var sessionToRestoreID: String?
    @ObservationIgnored private var configRequestID: String?
    @ObservationIgnored private var defaultConfigRequestID: String?
    @ObservationIgnored private var approvalRequestID: String?
    @ObservationIgnored private var directoryRequestID: String?
    @ObservationIgnored private var gitDiffRequestID: String?
    private var gitBranchRequestID: String?
    @ObservationIgnored private var credentialRequestID: String?
    @ObservationIgnored private var pairingCodeRequestID: String?
    @ObservationIgnored private var pairingCodeExpiryTask: Task<Void, Never>?
    @ObservationIgnored private var providerLoginRequestID: String?
    @ObservationIgnored private var providerRegistrationRequestID: String?
    @ObservationIgnored private var providerRegistrationTarget: ConfigurationTarget?
    @ObservationIgnored private var cronRequestIDs: Set<String> = []
    @ObservationIgnored private var toastDismissTask: Task<Void, Never>?
    @ObservationIgnored private var isChatVisible = false
    @ObservationIgnored private var latestSequence: UInt64?
    @ObservationIgnored private var sessionOpenCursor: UInt64?
    @ObservationIgnored private var steeringSubmissionID: String?
    @ObservationIgnored private var previewSelections: [String: FrontendPickerOption] = [:]

    init(
        client: GatewayClient? = nil,
        store: GatewayStore? = nil,
        requestSender: (@MainActor @Sendable (GatewayRequest) async throws -> Void)? = nil
    ) {
        let client = client ?? GatewayClient()
        let store = store ?? GatewayStore()
        self.client = client
        self.store = store
        self.requestSender = requestSender ?? { request in
            try await client.send(request)
        }
        self.accounts = store.loadAccounts()
        self.selectedAccountID = store.selectedAccountID()
        self.theme = ThemePreference(rawValue: UserDefaults.standard.string(forKey: "theme") ?? "") ?? .system
        if selectedAccountID == nil { selectedAccountID = accounts.first?.id }
        showsPairing = accounts.isEmpty
        #if DEBUG
        let environment = ProcessInfo.processInfo.environment
        if accounts.isEmpty,
           let endpoint = environment["HORUS_PAIR_ENDPOINT"],
           let code = environment["HORUS_PAIR_CODE"] {
            pairingEndpoint = endpoint
            pairingCode = code
        }
        switch ProcessInfo.processInfo.environment["HORUS_PAGE"] {
        case "gateway": destination = .gateway
        case "providers": destination = .providers
        case "agent": destination = .agent
        case "cron": destination = .cron
        case "profile": destination = .profile
        default: break
        }
        #endif
    }

    deinit {
        eventTask?.cancel()
        pairingCodeExpiryTask?.cancel()
        toastDismissTask?.cancel()
    }

    var selectedAccount: GatewayAccount? {
        accounts.first { $0.id == selectedAccountID }
    }

    var canOpenSession: Bool {
        connectionState.isReady
            && activeTurnID == nil
            && pendingApproval == nil
            && pendingDrafts.isEmpty
            && sessionRequestID == nil
            && sessionMutationRequestID == nil
            && gitBranchRequestID == nil
            && applyState != .applying
            && applyState != .restarting
    }

    var canCreateSession: Bool { canOpenSession }

    var isSwitchingGitBranch: Bool { gitBranchRequestID != nil }

    var runningSessionIDs: Set<String> {
        Set(sessions.lazy.filter { $0.activity.state != .idle }.map(\.sessionId))
    }

    var isApplyingConfiguration: Bool {
        configRequestID != nil
            || defaultConfigRequestID != nil
            || providerRegistrationRequestID != nil
            || applyState == .applying
            || applyState == .restarting
    }

    var contextFillFraction: Double {
        guard let modelContextWindow, modelContextWindow > 0 else { return 0 }
        return min(max(Double(contextTokens) / Double(modelContextWindow), 0), 1)
    }

    var contextFillPercent: Int {
        Int((contextFillFraction * 100).rounded())
    }

    /// How long the current turn has been running. Zero once the turn ends: the gateway only
    /// advertises `startedAt` while a turn is in flight.
    func sessionElapsed(at date: Date) -> TimeInterval {
        guard let session = sessions.first(where: { $0.sessionId == selectedSessionID }),
              session.activity.state != .idle,
              let startedAt = session.activity.startedAt
        else { return 0 }
        return max(0, date.timeIntervalSince1970 - TimeInterval(startedAt))
    }

    func showToast(_ message: String, tone: ToastTone = .info) {
        let toast = AppToast(message: message, tone: tone)
        toastDismissTask?.cancel()
        self.toast = toast
        let duration: Duration = tone == .error || tone == .warning ? .seconds(7) : .seconds(4)
        toastDismissTask = Task { [weak self] in
            try? await Task.sleep(for: duration)
            guard !Task.isCancelled, self?.toast?.id == toast.id else { return }
            self?.toast = nil
            self?.toastDismissTask = nil
        }
    }

    func dismissToast() {
        toastDismissTask?.cancel()
        toastDismissTask = nil
        toast = nil
    }

    func setChatVisible(_ visible: Bool) {
        isChatVisible = visible
        if visible, let selectedSessionID {
            unreadSessionIDs.remove(selectedSessionID)
        }
    }

    var capabilityCommands: [MountedCommand] {
        contributions.flatMap { contribution in
            contribution.commands.map {
                MountedCommand(capability: contribution.capability, command: $0)
            }
        }
    }

    var capabilityReferences: [MountedReference] {
        contributions.flatMap { contribution in
            contribution.references.map {
                MountedReference(capability: contribution.capability, reference: $0)
            }
        }
    }

    var middlewareContributionCounts: [MiddlewareContributionCount] {
        contributions.compactMap { contribution in
            guard let value = contribution.count,
                  let feature = middlewareFeatures.first(where: {
                      $0.id == contribution.capability
                  })
            else { return nil }
            return MiddlewareContributionCount(
                id: contribution.capability,
                label: feature.label,
                value: value
            )
        }
    }

    var currentSessionTitle: String {
        selectedSessionID.map(sessionTitle) ?? "New conversation"
    }

    private func sessionTitle(_ sessionID: String) -> String {
        let session = sessions.first(where: { $0.sessionId == sessionID })
        guard let message = (session?.title ?? session?.firstUserMessage)?
            .trimmingCharacters(in: .whitespacesAndNewlines),
            !message.isEmpty
        else { return "New conversation" }
        return String(message.prefix(72))
    }

    var headerWidgets: [MountedWidget] { widgets(in: "header") }
    var composerHeaderWidgets: [MountedWidget] { widgets(in: "composer_header") }
    var composerFooterWidgets: [MountedWidget] { widgets(in: "composer_footer") }

    func referenceSuggestions(in text: String, cursor: String.Index) -> ReferenceSuggestions? {
        guard text.indices.contains(cursor) || cursor == text.endIndex else { return nil }
        let start = text[..<cursor].lastIndex(where: { $0.isWhitespace })
            .map { text.index(after: $0) } ?? text.startIndex
        guard start < cursor, let trigger = text[start..<cursor].first else { return nil }
        let queryStart = text.index(after: start)
        let query = text[queryStart..<cursor]
        let matches = capabilityReferences
            .filter {
                $0.reference.trigger == trigger
                    && (query.isEmpty || $0.reference.value.localizedCaseInsensitiveContains(query))
            }
            .prefix(8)
        guard !matches.isEmpty else { return nil }
        return ReferenceSuggestions(range: start..<cursor, matches: Array(matches))
    }

    func start() {
        guard let account = selectedAccount else {
            #if DEBUG
            if !pairingCode.isEmpty, !pairingEndpoint.isEmpty { pair(); return }
            #endif
            showsPairing = true
            return
        }
        connect(to: account)
    }

    func applyPairingSetup(_ rawValue: String) {
        prefillPairing { try GatewayPairingSetup(rawValue) }
    }

    func applyPairingURL(_ url: URL) {
        prefillPairing { try GatewayPairingSetup(url: url) }
    }

    private func prefillPairing(_ parse: () throws -> GatewayPairingSetup) {
        showsPairing = true
        do {
            let setup = try parse()
            pairingEndpoint = setup.endpoint.rawValue
            pairingCode = setup.code
            pairingError = nil
        } catch {
            pairingError = error.localizedDescription
        }
    }

    func pair() {
        pairingError = nil
        do {
            let endpoint = try GatewayEndpoint(pairingEndpoint)
            let code = pairingCode.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !code.isEmpty else {
                let message = "Enter the one-time code shown by the gateway."
                pairingError = message
                showToast(message, tone: .error)
                return
            }
            let account = accounts.first(where: { $0.endpoint == endpoint })
                ?? GatewayAccount(endpoint: endpoint)
            let sameGateway = account.id == selectedAccountID
            let sessionID = sameGateway ? selectedSessionID : nil
            let generation = resetGatewayState(
                preservingDrafts: sameGateway,
                preservingSession: sessionID != nil
            )
            sessionToRestoreID = sessionID
            pendingPairingAccount = account
            beginConnection(to: endpoint, generation: generation) { [weak self] in
                guard let self, self.connectionGeneration == generation else { return }
                try await self.client.send(.pair(
                    code: code,
                    clientLabel: "Horus Apple",
                    clientKind: .currentApplePlatform
                ))
            }
        } catch {
            pairingError = error.localizedDescription
            showToast(error.localizedDescription, tone: .error)
        }
    }

    func selectAccount(_ id: UUID?) {
        guard let id, let account = accounts.first(where: { $0.id == id }) else { return }
        connect(to: account)
    }

    func reconnect() {
        guard let account = selectedAccount else { return }
        connect(to: account)
    }

    func repairSelectedGateway() {
        guard let account = selectedAccount else {
            showsPairing = true
            return
        }
        pairingEndpoint = account.endpoint.rawValue
        pairingCode = ""
        pairingError = "Enter a new one-time code to repair this pairing."
        showsPairing = true
    }

    func chooseWorkspace(_ selectedPath: String) {
        let path = selectedPath.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !path.isEmpty else {
            workspaceError = "Choose a folder on the gateway host."
            return
        }
        guard canCreateSession else { return }
        sessionToRestoreID = nil
        sessionOpenCursor = nil
        let id = requestID("create")
        sessionRequestID = id
        workspaceError = nil
        isChangingWorkspace = true
        connectionState = .loading
        transmit(.createSession(requestID: id, workspace: path)) { [weak self] message in
            self?.sessionRequestID = nil
            self?.isChangingWorkspace = false
            self?.connectionState = .ready
            self?.workspaceError = message
        }
    }

    func openWorkspaceBrowser() {
        guard canCreateSession else { return }
        showsWorkspaceBrowser = true
        loadDirectory(workspace?.path ?? "/")
    }

    func loadDirectory(_ path: String) {
        let id = requestID("directories")
        directoryRequestID = id
        directoryError = nil
        isLoadingDirectories = true
        transmit(.listDirectories(requestID: id, path: path, includeFiles: false)) { [weak self] message in
            guard self?.directoryRequestID == id else { return }
            self?.directoryRequestID = nil
            self?.isLoadingDirectories = false
            self?.directoryError = message
        }
    }

    func forgetSelectedGateway() {
        guard let account = selectedAccount else { return }
        do {
            try store.remove(account)
            accounts.removeAll { $0.id == account.id }
            selectedAccountID = nil
            if let next = accounts.first {
                connect(to: next)
            } else {
                let generation = resetGatewayState(preservingDrafts: false)
                Task { [weak self] in
                    guard let self, self.connectionGeneration == generation else { return }
                    await self.client.disconnect()
                }
                showsPairing = true
            }
            showToast("Gateway removed.", tone: .info)
        } catch {
            showToast(error.localizedDescription, tone: .error)
        }
    }

    func openNewSession() {
        openWorkspaceBrowser()
    }

    func openSession(_ sessionID: String) {
        guard canOpenSession, sessionID != selectedSessionID else { return }
        requestSessionOpen(sessionID, lastSequence: nil)
    }

    private func restoreSession(_ sessionID: String) {
        let cursor = sessionID == selectedSessionID ? latestSequence : nil
        requestSessionOpen(sessionID, lastSequence: cursor)
    }

    private func requestSessionOpen(_ sessionID: String, lastSequence: UInt64?) {
        sessionToRestoreID = nil
        sessionOpenCursor = lastSequence
        let id = requestID("open")
        sessionRequestID = id
        connectionState = .loading
        transmit(.openSession(
            requestID: id,
            sessionID: sessionID,
            lastSequence: lastSequence
        )) { [weak self] _ in
            guard self?.sessionRequestID == id else { return }
            self?.sessionRequestID = nil
            self?.sessionOpenCursor = nil
            self?.connectionState = .ready
        }
    }

    // Renaming, pinning and deleting address a session by id, so they work on any chat in the
    // catalogue rather than only the open one.
    func renameSession(_ session: SessionRecord, title: String) {
        guard sessionMutationRequestID == nil else { return }
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return }
        let id = requestID("session-rename")
        sessionMutationRequestID = id
        transmit(.renameSession(
            requestID: id,
            sessionID: session.sessionId,
            title: title
        )) { [weak self] _ in
            if self?.sessionMutationRequestID == id { self?.sessionMutationRequestID = nil }
        }
    }

    func setSessionPinned(_ session: SessionRecord, pinned: Bool) {
        guard sessionMutationRequestID == nil else { return }
        let id = requestID("session-pin")
        sessionMutationRequestID = id
        transmit(.setSessionPinned(
            requestID: id,
            sessionID: session.sessionId,
            pinned: pinned
        )) { [weak self] _ in
            if self?.sessionMutationRequestID == id { self?.sessionMutationRequestID = nil }
        }
    }

    func deleteSession(_ session: SessionRecord) {
        guard sessionMutationRequestID == nil else { return }
        let id = requestID("session-delete")
        sessionMutationRequestID = id
        transmit(.deleteSession(
            requestID: id,
            sessionID: session.sessionId
        )) { [weak self] _ in
            if self?.sessionMutationRequestID == id { self?.sessionMutationRequestID = nil }
        }
    }

    func refreshGitDiff() {
        guard connectionState.isReady, let sessionID = selectedSessionID else { return }
        let id = requestID("git-diff")
        gitDiffRequestID = id
        transmit(.getGitDiff(requestID: id, sessionID: sessionID))
    }

    func switchGitBranch(to branch: String) {
        guard canOpenSession,
              let sessionID = selectedSessionID,
              let gitStatus,
              branch != gitStatus.currentBranch,
              gitStatus.branches.contains(branch)
        else { return }
        let id = requestID("git-branch")
        gitBranchRequestID = id
        transmit(.switchGitBranch(requestID: id, sessionID: sessionID, branch: branch)) { [weak self] _ in
            if self?.gitBranchRequestID == id { self?.gitBranchRequestID = nil }
        }
    }

    func sendMessage() {
        guard let sessionID = selectedSessionID else { return }
        let text = composer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        guard text.utf8.count <= maximumComposerBytes else {
            showToast("Messages are limited to 1 MiB.", tone: .error)
            return
        }
        let id = requestID("input")
        let op: AgentOperation
        if let activeTurnID, let activeOperation {
            op = .activeInput(operation: activeOperation, turnID: activeTurnID, text: text)
            steeringQueued = true
            steeringSubmissionID = id
        } else {
            op = .userInput(text: text)
        }
        pendingDrafts[id] = text
        composer = ""
        transmit(.submit(sessionID: sessionID, submission: Submission(id: id, op: op))) { [weak self] _ in
            self?.restoreDraft(id: id)
            if self?.steeringSubmissionID == id {
                self?.steeringSubmissionID = nil
                self?.steeringQueued = false
            }
        }
    }

    func submitWidget(_ mounted: MountedWidget) {
        guard let sessionID = selectedSessionID, let action = mounted.widget.action else { return }
        let id = requestID("widget")
        transmit(.submit(sessionID: sessionID, submission: Submission(id: id, op: action)))
    }

    func submitPickerOption(_ option: FrontendPickerOption) {
        guard let sessionID = selectedSessionID else { return }
        let id = requestID("picker")
        pendingPicker = nil
        if case .capabilityCommand = option.op { previewSelections[id] = option }
        transmit(.submit(
            sessionID: sessionID,
            submission: Submission(id: id, op: option.op)
        )) { [weak self] _ in
            self?.previewSelections.removeValue(forKey: id)
        }
    }

    func selectModel(_ route: String) {
        guard let sessionID = selectedSessionID, route != selectedModelRoute else { return }
        transmit(.submit(
            sessionID: sessionID,
            submission: Submission(id: requestID("model"), op: .setModel(route: route))
        ))
    }

    func interrupt() {
        guard let sessionID = selectedSessionID, let activeTurnID else { return }
        transmit(.submit(
            sessionID: sessionID,
            submission: Submission(
                id: requestID("interrupt"),
                op: .interrupt(turnID: activeTurnID)
            )
        ))
    }

    func resolveApproval(_ decision: ReviewDecision) {
        guard let sessionID = selectedSessionID,
              let approval = pendingApproval,
              approvalRequestID == nil
        else { return }
        let id = requestID("approval")
        approvalRequestID = id
        transmit(.submit(
            sessionID: sessionID,
            submission: Submission(
                id: id,
                op: .execApproval(id: approval.id, decision: decision)
            )
        )) { [weak self] _ in
            guard self?.approvalRequestID == id else { return }
            self?.approvalRequestID = nil
        }
    }

    func selectArtifact(_ id: String) {
        selectedArtifactID = id
        showsInspector = true
        refreshGitDiff()
    }

    func showInspector() {
        showsInspector = true
        refreshGitDiff()
    }

    func toggleInspector() {
        if showsInspector {
            showsInspector = false
        } else {
            showInspector()
        }
    }

    func submitCommand(_ mounted: MountedCommand, arguments: String) {
        guard canOpenSession, let sessionID = selectedSessionID else { return }
        transmit(.submit(
            sessionID: sessionID,
            submission: Submission(
                id: requestID("command"),
                op: .capabilityCommand(
                    capability: mounted.capability,
                    command: mounted.command.name,
                    arguments: arguments
                )
            )
        ))
    }

    func changeAgentForCurrentChat() {
        applyAgentConfiguration(to: .session)
    }

    func saveAgentAsDefault() {
        applyAgentConfiguration(to: .defaultAgent)
    }

    private func applyAgentConfiguration(to target: ConfigurationTarget) {
        guard !isApplyingConfiguration, let draft = agentDraft else { return }
        let id = requestID("configure")
        applyState = .applying
        switch target {
        case .session:
            guard let sessionID = selectedSessionID, let snapshot = agentSnapshot else {
                applyState = .idle
                return
            }
            configRequestID = id
            transmit(.configureSession(
                requestID: id,
                sessionID: sessionID,
                expectedRevision: snapshot.revision,
                config: draft
            )) { [weak self] message in
                guard self?.configRequestID == id else { return }
                self?.configRequestID = nil
                self?.applyState = .failed(message)
            }
        case .defaultAgent:
            guard let snapshot = defaultAgentSnapshot else {
                applyState = .failed("The gateway has no default agent configuration.")
                return
            }
            defaultConfigRequestID = id
            transmit(.configureDefaultAgent(
                requestID: id,
                expectedRevision: snapshot.revision,
                config: draft
            )) { [weak self] message in
                guard self?.defaultConfigRequestID == id else { return }
                self?.defaultConfigRequestID = nil
                self?.applyState = .failed(message)
            }
        }
    }

    func setApprovalPolicy(_ policy: ApprovalPolicy) {
        guard let snapshot = agentSnapshot, let draft = agentDraft else { return }
        guard draft == snapshot.config else {
            showToast(
                "Apply or reload pending agent/provider edits before changing approval.",
                tone: .warning
            )
            return
        }
        guard draft.approval != policy else { return }
        agentDraft?.approval = policy
        changeAgentForCurrentChat()
    }

    func reloadAgentDraft() {
        agentDraft = agentSnapshot?.config
        applyState = .idle
        showToast("Agent draft reloaded.", tone: .info)
    }

    func selectProvider(_ provider: String) {
        guard let status = providerStatuses.first(where: { $0.provider == provider }),
              let webSearch = status.webSearch.first
        else { return }
        let selectedModel = status.models.first
        agentDraft?.provider = ProviderConfig(
            provider: status.provider,
            model: selectedModel?.id ?? "",
            baseUrl: status.defaultBaseUrl,
            reasoningEffort: selectedModel?.defaultReasoning,
            webSearch: webSearch
        )
        providerAPIKey = ""
        providerActionState = .idle
    }

    func selectProviderModel(_ modelID: String) {
        guard let status = providerStatuses.first(where: {
            $0.provider == agentDraft?.provider.provider
        }) else { return }
        agentDraft?.provider.model = modelID
        agentDraft?.provider.reasoningEffort = status.models
            .first(where: { $0.id == modelID })?
            .defaultReasoning
    }

    func saveProviderCredential(provider: String) {
        let key = providerAPIKey
        guard !key.isEmpty else {
            let message = "Enter an API key. It will be sent once and never read back."
            providerActionState = .failed(message)
            showToast(message, tone: .error)
            return
        }
        let id = requestID("credential")
        credentialRequestID = id
        providerActionState = .savingCredential(provider)
        let request: GatewayRequest
        if let baseURL = agentDraft?.provider.baseUrl {
            request = .setProviderEndpointCredential(
                requestID: id,
                provider: provider,
                baseURL: baseURL,
                apiKey: key
            )
        } else {
            request = .setProviderCredential(requestID: id, provider: provider, apiKey: key)
        }
        transmit(request) { [weak self] message in
            self?.providerActionState = .failed(message)
        }
    }

    func changeProviderForCurrentChat() {
        registerProvider(for: .session)
    }

    func saveProviderAsDefault() {
        registerProvider(for: .defaultAgent)
    }

    private func registerProvider(for target: ConfigurationTarget) {
        guard selectedSessionID != nil, let config = agentDraft?.provider else { return }
        let id = requestID("provider")
        providerRegistrationRequestID = id
        providerRegistrationTarget = target
        applyState = .applying
        transmit(.registerProvider(requestID: id, config: config)) { [weak self] message in
            guard self?.providerRegistrationRequestID == id else { return }
            self?.providerRegistrationRequestID = nil
            self?.providerRegistrationTarget = nil
            self?.applyState = .failed(message)
        }
    }

    func startProviderLogin(provider: String) {
        let id = requestID("login")
        providerLoginRequestID = id
        providerActionState = .startingLogin(provider)
        transmit(.startProviderLogin(requestID: id, provider: provider)) { [weak self] message in
            self?.providerActionState = .failed(message)
        }
    }

    func createPairingCode() {
        let id = requestID("pairing-code")
        pairingCodeRequestID = id
        pairingCodeExpiryTask?.cancel()
        pairingCodeExpiryTask = nil
        pairingCodeInfo = nil
        transmit(.createPairingCode(requestID: id)) { [weak self] _ in
            self?.pairingCodeRequestID = nil
        }
    }

    func startCronSetup() {
        guard let sessionID = selectedSessionID else { return }
        let task = cronTaskDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        let id = requestID("cron-setup")
        cronRequestIDs.insert(id)
        cronError = nil
        destination = .chat
        transmit(.startCronSetup(
            requestID: id,
            sessionID: sessionID,
            task: task.isEmpty ? nil : task
        )) { [weak self] message in
            self?.cronRequestIDs.remove(id)
            self?.cronError = message
        }
    }

    func rescheduleCron(_ task: CronTask, schedule: String) {
        guard let sessionID = selectedSessionID else { return }
        let value = schedule.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        let request = requestID("cron-reschedule")
        cronRequestIDs.insert(request)
        transmit(.rescheduleCron(
            requestID: request,
            sessionID: sessionID,
            id: task.id,
            schedule: value
        )) { [weak self] message in
            self?.cronRequestIDs.remove(request)
            self?.cronError = message
        }
    }

    func deleteCron(_ task: CronTask) {
        guard let sessionID = selectedSessionID else { return }
        let request = requestID("cron-delete")
        cronRequestIDs.insert(request)
        transmit(.deleteCron(requestID: request, sessionID: sessionID, id: task.id)) { [weak self] message in
            self?.cronRequestIDs.remove(request)
            self?.cronError = message
        }
    }

    func runCron(_ task: CronTask) {
        guard let sessionID = selectedSessionID else { return }
        let request = requestID("cron-run")
        cronRequestIDs.insert(request)
        transmit(.runCron(requestID: request, sessionID: sessionID, id: task.id)) { [weak self] message in
            self?.cronRequestIDs.remove(request)
            self?.cronError = message
        }
    }

    func refreshCron() {
        guard let sessionID = selectedSessionID else { return }
        transmit(.listCron(requestID: requestID("cron-list"), sessionID: sessionID))
        transmit(.listCronHistory(
            requestID: requestID("cron-history"),
            sessionID: sessionID,
            id: nil
        ))
    }

    func setTheme(_ theme: ThemePreference) {
        self.theme = theme
        UserDefaults.standard.set(theme.rawValue, forKey: "theme")
    }

    private func connect(to account: GatewayAccount) {
        let sameGateway = account.id == selectedAccountID
        let sessionID = sameGateway ? selectedSessionID : nil
        let generation = resetGatewayState(
            preservingDrafts: sameGateway,
            preservingSession: sessionID != nil
        )
        sessionToRestoreID = sessionID
        selectedAccountID = account.id
        store.select(account)
        connectionState = .connecting
        Task { [weak self] in
            guard let self, self.connectionGeneration == generation else { return }
            await self.client.disconnect()
            guard self.connectionGeneration == generation else { return }
            do {
                let token = try self.store.token(for: account)
                self.beginConnection(to: account.endpoint, generation: generation) { [weak self] in
                    guard let self, self.connectionGeneration == generation else { return }
                    try await self.client.send(.authenticate(
                        token: token,
                        clientKind: .currentApplePlatform
                    ))
                }
            } catch {
                self.connectionState = .failed(error.localizedDescription)
                self.showToast(error.localizedDescription, tone: .error)
                if let storeError = error as? GatewayStore.StoreError,
                   case .missingToken = storeError {
                    self.repairSelectedGateway()
                }
            }
        }
    }

    private func beginConnection(
        to endpoint: GatewayEndpoint,
        generation: UUID,
        authenticate: @escaping @MainActor @Sendable () async throws -> Void
    ) {
        connectionState = .connecting

        Task { [weak self] in
            guard let self else { return }
            do {
                let stream = try await self.client.connect(to: endpoint)
                guard generation == self.connectionGeneration else { return }
                self.connectionState = .authenticating
                self.eventTask = Task { [weak self] in
                    do {
                        for try await frame in stream {
                            guard let self, generation == self.connectionGeneration else { return }
                            self.handle(frame)
                        }
                        self?.connectionEnded(generation: generation, message: "The gateway closed the connection.")
                    } catch {
                        self?.connectionEnded(generation: generation, message: error.localizedDescription)
                    }
                }
                try await authenticate()
            } catch {
                self.connectionEnded(generation: generation, message: error.localizedDescription)
            }
        }
    }

    private func transmit(
        _ request: GatewayRequest,
        onFailure: (@MainActor (String) -> Void)? = nil
    ) {
        let generation = connectionGeneration
        Task { [weak self] in
            guard let self, generation == self.connectionGeneration else { return }
            do {
                try await self.requestSender(request)
            } catch {
                guard generation == self.connectionGeneration else { return }
                let message = error.localizedDescription
                self.showToast(message, tone: .error)
                onFailure?(message)
            }
        }
    }

    private func handle(_ envelope: GatewayEnvelope) {
        switch envelope {
        case .paired(_, let token):
            guard let account = pendingPairingAccount else { return }
            do {
                try store.save(account, token: token)
                accounts = store.loadAccounts()
                selectedAccountID = account.id
                pendingPairingAccount = nil
                pairingCode = ""
                showsPairing = false
                showToast("Gateway paired.", tone: .success)
            } catch {
                pairingError = error.localizedDescription
                showToast(error.localizedDescription, tone: .error)
            }
        case .authenticated:
            connectionState = .loading
        case .ready(let payload):
            applyGatewayReady(payload)
        case .sessionOpened(let requestID, let payload):
            guard requestID == sessionRequestID else { break }
            applySessionReady(payload, opened: true)
        case .sessionChanged(let payload):
            guard payload.session.sessionId == selectedSessionID,
                  payload.config.revision >= (agentSnapshot?.revision ?? 0)
            else { break }
            applySessionReady(payload, opened: false)
        case .gatewayConfigured(let requestID, let payload):
            applyGatewayConfigurationResponse(requestID: requestID, payload: payload)
        case .accepted(let requestID):
            handleAccepted(requestID)
        case .rejected(let rejection):
            handleRejected(rejection)
        case .agentEvent(let sessionID, let sequence, let event, let blocks, let history, let preview):
            guard sessionID == selectedSessionID else { break }
            guard latestSequence.map({ sequence > $0 }) ?? true else { return }
            latestSequence = sequence
            reduce(event: event, blocks: blocks, history: history, preview: preview)
        case .sessions(let requestID, let sessions):
            if requestID == sessionMutationRequestID { sessionMutationRequestID = nil }
            applySessions(sessions)
        case .clients:
            break
        case .providerCredentialStatus(let requestID, let provider, let configured):
            if let index = providerStatuses.firstIndex(where: { $0.provider == provider }) {
                providerStatuses[index].configured = configured
            }
            if requestID == credentialRequestID {
                credentialRequestID = nil
                if configured {
                    providerAPIKey = ""
                    providerActionState = .credentialSaved(provider)
                    showToast("\(provider) credential saved.", tone: .success)
                } else {
                    let message = "The gateway did not store the provider credential."
                    providerActionState = .failed(message)
                    showToast(message, tone: .error)
                }
            }
        case .pairingCode(let requestID, let code, let expiresAt):
            guard requestID == pairingCodeRequestID else { break }
            pairingCodeRequestID = nil
            setPairingCode(
                code,
                expiresAt: Date(timeIntervalSince1970: TimeInterval(expiresAt))
            )
        case .providerLoginStarted(let requestID, _, let provider, let url, let code):
            guard requestID == providerLoginRequestID else { break }
            providerActionState = .deviceCode(
                provider: provider,
                url: url,
                code: code
            )
        case .providerLoginFinished(let requestID, _, let provider):
            if requestID == providerLoginRequestID {
                providerLoginRequestID = nil
                providerActionState = .loginFinished(provider)
                showToast("Signed in to \(provider).", tone: .success)
            }
            if let index = providerStatuses.firstIndex(where: { $0.provider == provider }) {
                providerStatuses[index].configured = true
            }
        case .profile(_, let profile):
            self.profile = profile
        case .artifacts(_, let sessionID, let artifacts):
            guard sessionID == selectedSessionID else { break }
            self.artifacts = artifacts
            if selectedArtifactID == nil || !self.artifacts.contains(where: { $0.id == selectedArtifactID }) {
                selectedArtifactID = self.artifacts.first?.id
            }
        case .gitDiff(let requestID, let sessionID, let diff):
            guard requestID == gitDiffRequestID, sessionID == selectedSessionID else { break }
            gitDiffRequestID = nil
            gitDiff = diff
        case .directories(let requestID, let listing):
            guard requestID == directoryRequestID else { break }
            directoryRequestID = nil
            directoryListing = listing
            directoryError = nil
            isLoadingDirectories = false
        case .cronTasks(let requestID, let sessionID, let tasks):
            guard sessionID == selectedSessionID else { break }
            cronRequestIDs.remove(requestID)
            cronTasks = tasks
        case .cronHistory(let requestID, let sessionID, let runs):
            guard sessionID == selectedSessionID else { break }
            cronRequestIDs.remove(requestID)
            cronRuns = runs
        case .error(let failure):
            if pendingPairingAccount != nil { pairingError = failure.message }
            if failure.code == "unauthorized", pendingPairingAccount == nil {
                repairSelectedGateway()
            }
            showToast(failure.message, tone: .error)
            if failure.fatal {
                restorePendingDrafts()
                connectionState = .failed(failure.message)
            }
        }
    }

    private func applyGatewayReady(_ payload: ReadyPayload) {
        applyGatewayCatalog(payload)
        if sessionRequestID == nil { connectionState = .ready }
        applySessions(payload.sessions)
        transmit(.getProfile(requestID: requestID("profile")))
        guard sessionRequestID == nil else { return }
        if let sessionToRestoreID {
            if let session = sessions.first(where: { $0.sessionId == sessionToRestoreID }) {
                restoreSession(session.sessionId)
            } else {
                showToast("The previously selected chat is no longer available.", tone: .error)
                clearSelectedSession()
            }
        } else if selectedSessionID == nil, let session = sessions.first {
            openSession(session.sessionId)
        }
    }

    func applyGatewayConfigurationResponse(
        requestID: String,
        payload: ReadyPayload
    ) {
        applyGatewayReady(payload)
        if requestID == providerRegistrationRequestID {
            let target = providerRegistrationTarget
            providerRegistrationRequestID = nil
            providerRegistrationTarget = nil
            applyState = .idle
            if let target { applyAgentConfiguration(to: target) }
        } else if requestID == defaultConfigRequestID {
            defaultConfigRequestID = nil
            applyState = .applied
            showToast("Default agent saved for new chats.", tone: .success)
        }
    }

    func applyGatewayCatalog(_ payload: ReadyPayload) {
        providerStatuses = payload.providers
        modelChoices = payload.models
        middlewareFeatures = payload.middlewareFeatures
        defaultAgentSnapshot = payload.defaultConfig
    }

    private func applySessionReady(
        _ payload: SessionReadyPayload,
        opened: Bool
    ) {
        if selectedSessionID != payload.session.sessionId {
            restorePendingDrafts()
            resetSessionState()
        }
        if opened {
            latestSequence = sessionOpenCursor
            sessionOpenCursor = nil
        }
        sessionRequestID = nil
        workspace = payload.workspace
        gitStatus = payload.git
        workspaceError = nil
        isChangingWorkspace = false
        showsWorkspaceBrowser = false
        selectedSessionID = payload.session.sessionId
        if isChatVisible { unreadSessionIDs.remove(payload.session.sessionId) }
        selectedModelRoute = payload.session.model.route
        modelContextWindow = payload.session.model.modelContextWindow
        contributions = payload.contributions
        toolCount = payload.toolCount
        mountedWidgets = payload.contributions.flatMap { contribution in
            contribution.widgets.map { MountedWidget(capability: contribution.capability, widget: $0) }
        }
        activeOperation = payload.contributions.compactMap(\.activeInput?.operation).first
        agentDraft = refreshedAgentDraft(
            currentDraft: agentDraft,
            currentSnapshot: agentSnapshot,
            incomingSnapshot: payload.config
        )
        agentSnapshot = payload.config
        connectionState = .ready
        if applyState == .restarting {
            applyState = .applied
            showToast("Agent configuration applied.", tone: .success)
        }
        if opened { requestSessionData() }
    }

    func applySessions(_ records: [SessionRecord]) {
        let previous = Dictionary(uniqueKeysWithValues: sessions.map { ($0.sessionId, $0.activity) })
        sessions = records.filter(\.catalogVisible)
        for session in sessions {
            applyActivityTransition(
                from: previous[session.sessionId],
                to: session.activity,
                sessionID: session.sessionId
            )
        }
        let visible = Set(sessions.map(\.sessionId))
        unreadSessionIDs.formIntersection(visible)
        guard let selectedSessionID,
              !sessions.contains(where: { $0.sessionId == selectedSessionID }),
              sessionRequestID == nil
        else { return }
        if let next = sessions.first {
            openSession(next.sessionId)
        } else {
            clearSelectedSession()
        }
    }

    private func applyActivityTransition(
        from previous: SessionActivity?,
        to activity: SessionActivity,
        sessionID: String
    ) {
        guard let previous, previous != activity else { return }
        if activity.state == .awaitingApproval,
           previous.state != .awaitingApproval {
            showToast("\(sessionTitle(sessionID)) needs approval.", tone: .warning)
        }
        guard activity.state == .idle,
              let outcome = activity.lastOutcome,
              previous.state != .idle
                || previous.lastOutcome != outcome
                || previous.message != activity.message
        else { return }

        let isActiveChat = selectedSessionID == sessionID && isChatVisible
        if isActiveChat {
            unreadSessionIDs.remove(sessionID)
        } else {
            unreadSessionIDs.insert(sessionID)
        }

        switch outcome {
        case .completed:
            guard !isActiveChat else { return }
            showToast("\(sessionTitle(sessionID)) is ready.", tone: .success)
        case .aborted:
            guard !isActiveChat else { return }
            let detail = activity.message.map { ": \($0)" } ?? ""
            showToast("\(sessionTitle(sessionID)) stopped\(detail).", tone: .warning)
        case .failed:
            let detail = activity.message.map { ": \($0)" } ?? ""
            showToast("\(sessionTitle(sessionID)) failed\(detail).", tone: .error)
        }
    }

    private func requestSessionData() {
        guard selectedSessionID != nil else { return }
        refreshArtifacts()
        refreshGitDiff()
        refreshCron()
    }

    private func refreshArtifacts() {
        guard let sessionID = selectedSessionID else { return }
        transmit(.listArtifacts(requestID: requestID("artifacts"), sessionID: sessionID))
    }

    private func clearSelectedSession() {
        latestSequence = nil
        sessionOpenCursor = nil
        selectedSessionID = nil
        resetSessionState()
        connectionState = .ready
    }

    private func handleAccepted(_ requestID: String) {
        if requestID == approvalRequestID {
            pendingApproval = nil
            approvalRequestID = nil
        }
        if requestID == configRequestID {
            applyState = .restarting
            configRequestID = nil
        }
        if requestID == sessionMutationRequestID {
            transmit(.listSessions(requestID: requestID)) { [weak self] _ in
                if self?.sessionMutationRequestID == requestID {
                    self?.sessionMutationRequestID = nil
                }
            }
        }
        if requestID == gitBranchRequestID {
            gitBranchRequestID = nil
            showToast("Git branch changed.", tone: .success)
            refreshGitDiff()
        }
        if cronRequestIDs.remove(requestID) != nil {
            cronTaskDraft = ""
            refreshCron()
        }
    }

    private func handleRejected(_ rejection: GatewayRejection) {
        if pendingDrafts[rejection.requestId] != nil {
            restoreDraft(id: rejection.requestId)
        }
        if rejection.requestId == configRequestID
            || rejection.requestId == defaultConfigRequestID {
            switch rejection.code {
            case "revision_conflict": applyState = .conflict(rejection.message)
            case "agent_busy": applyState = .busy(rejection.message)
            case "invalid_config": applyState = .invalid(rejection.message)
            default: applyState = .failed(rejection.message)
            }
            if rejection.requestId == configRequestID { configRequestID = nil }
            if rejection.requestId == defaultConfigRequestID { defaultConfigRequestID = nil }
        }
        if rejection.requestId == approvalRequestID {
            approvalRequestID = nil
        }
        if rejection.requestId == sessionRequestID {
            sessionRequestID = nil
            sessionOpenCursor = nil
            connectionState = .ready
            if isChangingWorkspace { workspaceError = rejection.message }
            isChangingWorkspace = false
        }
        if rejection.requestId == sessionMutationRequestID {
            sessionMutationRequestID = nil
        }
        if rejection.requestId == directoryRequestID {
            directoryError = rejection.message
            directoryRequestID = nil
            isLoadingDirectories = false
        }
        if rejection.requestId == gitDiffRequestID {
            gitDiffRequestID = nil
        }
        if rejection.requestId == gitBranchRequestID {
            gitBranchRequestID = nil
        }
        if rejection.requestId == credentialRequestID {
            providerActionState = .failed(rejection.message)
            credentialRequestID = nil
        }
        if rejection.requestId == providerLoginRequestID {
            providerActionState = .failed(rejection.message)
            providerLoginRequestID = nil
        }
        if rejection.requestId == providerRegistrationRequestID {
            applyState = .failed(rejection.message)
            providerRegistrationRequestID = nil
            providerRegistrationTarget = nil
        }
        if rejection.requestId == pairingCodeRequestID {
            pairingCodeRequestID = nil
        }
        if cronRequestIDs.remove(rejection.requestId) != nil {
            cronError = rejection.message
        }
        showToast(
            rejection.message,
            tone: rejection.code == "revision_conflict" || rejection.code == "agent_busy"
                ? .warning
                : .error
        )
        if rejection.fatal {
            restorePendingDrafts()
            connectionState = .failed(rejection.message)
        }
    }

    func reduce(
        event: AgentEventRecord,
        blocks: [FrontendBlock],
        history: [RenderedEventRecord]? = nil,
        preview: RenderedPreview?
    ) {
        let type = event.msg["type"]?.stringValue ?? "unknown"
        let wasRendered = !blocks.isEmpty
        if let submissionID = event.submissionId {
            if type == "warning" || type == "error" {
                if let draft = pendingDrafts.removeValue(forKey: submissionID) { restoreDraft(draft) }
                previewSelections.removeValue(forKey: submissionID)
            } else if type == "user_message" {
                pendingDrafts.removeValue(forKey: submissionID)
                if submissionID == steeringSubmissionID {
                    steeringSubmissionID = nil
                    steeringQueued = false
                }
            }
        }

        if !blocks.isEmpty {
            for block in blocks { apply(block) }
        }
        if let preview {
            apply(
                preview,
                selection: event.submissionId.flatMap { previewSelections.removeValue(forKey: $0) }
            )
        }

        switch type {
        case "session_history":
            for rendered in history ?? [] {
                guard rendered.event["frontendType"]?.stringValue != "picker" else { continue }
                reduce(
                    event: AgentEventRecord(submissionId: nil, msg: rendered.event),
                    blocks: rendered.blocks,
                    preview: nil
                )
            }
        case "user_message":
            appendText(event.msg["message"]?.stringValue, kind: .user)
        case "agent_message_content_delta":
            let phase = event.msg["phase"]?.stringValue
            guard let itemID = event.msg["itemId"]?.stringValue else { return }
            appendStream(
                id: itemID,
                delta: event.msg["delta"]?.stringValue ?? "",
                kind: phase == "commentary" ? .event : .assistant
            )
        case "agent_reasoning_content_delta":
            guard let itemID = event.msg["itemId"]?.stringValue else { return }
            appendStream(
                id: "reasoning-\(itemID)",
                delta: event.msg["delta"]?.stringValue ?? "",
                kind: .reasoning
            )
        case "agent_message":
            let phase = event.msg["phase"]?.stringValue
            let kind: TranscriptEntry.Kind = phase == "commentary" ? .event : .assistant
            if wasRendered {
                transcript.removeAll { $0.pending && $0.kind == kind }
            } else {
                completeStream(text: event.msg["message"]?.stringValue ?? "", kind: kind)
            }
        case "task_started":
            activeTurnID = event.msg["turnId"]?.stringValue
            if let window = event.msg["modelContextWindow"]?.intValue {
                modelContextWindow = Int64(window)
            }
        case "task_complete":
            finishPendingTranscriptEntries()
            activeTurnID = nil
            refreshGitDiff()
            steeringQueued = false
            steeringSubmissionID = nil
            pendingApproval = nil
            approvalRequestID = nil
        case "web_search_begin":
            appendText("Searching the web", kind: .event, tone: "warning")
        case "web_search_end":
            let query = event.msg["query"]?.stringValue
                ?? event.msg["action"]?.stringValue
                ?? "Web search complete"
            appendText("Searched: \(query)", kind: .event, tone: "success")
        case "turn_aborted":
            finishPendingTranscriptEntries()
            activeTurnID = nil
            refreshGitDiff()
            steeringQueued = false
            steeringSubmissionID = nil
            pendingApproval = nil
            approvalRequestID = nil
            appendText(
                "Turn aborted: \(event.msg["reason"]?.stringValue ?? "Unknown reason")",
                kind: .event,
                tone: "warning"
            )
        case "warning":
            appendText(event.msg["message"]?.stringValue, kind: .event, tone: "warning")
        case "error":
            let message = event.msg["message"]?.stringValue ?? "Agent error"
            appendText(message, kind: .error, tone: "error")
        case "model_changed":
            selectedModelRoute = event.msg["route"]?.stringValue ?? selectedModelRoute
            if let window = event.msg["modelContextWindow"]?.intValue {
                modelContextWindow = Int64(window)
            }
        case "session_resume_requested":
            if let sessionID = event.msg["sessionId"]?.stringValue { openSession(sessionID) }
        case "exec_approval_request":
            approvalRequestID = nil
            pendingApproval = decodeApproval(event.msg)
        case "token_count":
            if let usage = event.msg["info"]?["totalTokenUsage"],
               let decoded = TokenUsage(json: usage) {
                currentUsage = decoded
            }
            if let usage = event.msg["info"]?["lastTokenUsage"],
               let latest = TokenUsage(json: usage) {
                lastUsage = latest
                contextTokens = max(
                    0,
                    max(latest.totalTokens, latest.inputTokens + latest.outputTokens)
                )
            }
            if let window = event.msg["info"]?["modelContextWindow"]?.intValue {
                modelContextWindow = Int64(window)
            }
        case "frontend":
            applyFrontendEvent(event.msg)
        default:
            break
        }
    }

    private func applyFrontendEvent(_ event: JSONValue) {
        switch event["frontendType"]?.stringValue {
        case "render":
            guard let block = renderedBlock(from: event) else { return }
            apply(block)
        case "widget":
            guard let capability = event["capability"]?.stringValue,
                  let item = event["item"],
                  let widget = try? FrontendWidget(json: item)
            else { return }
            let mounted = MountedWidget(capability: capability, widget: widget)
            if let index = mountedWidgets.firstIndex(where: { $0.id == mounted.id }) {
                mountedWidgets[index] = mounted
            } else {
                mountedWidgets.append(mounted)
            }
        case "remove_widget":
            guard let capability = event["capability"]?.stringValue,
                  let id = event["id"]?.stringValue
            else { return }
            mountedWidgets.removeAll { $0.capability == capability && $0.widget.id == id }
        case "picker":
            guard let title = event["title"]?.stringValue else { return }
            let options = event["options"]?.arrayValue?.compactMap {
                try? FrontendPickerOption(json: $0)
            } ?? []
            guard !options.isEmpty else { return }
            pendingPicker = FrontendPickerPrompt(title: title, options: options)
        default:
            break
        }
    }

    private func renderedBlock(from event: JSONValue) -> FrontendBlock? {
        guard event["type"]?.stringValue == "frontend",
              event["frontendType"]?.stringValue == "render",
              let capability = event["capability"]?.stringValue,
              let value = event["block"],
              let block = try? FrontendBlock(json: value)
        else { return nil }
        return block.namespaced(to: capability)
    }

    private func apply(_ block: FrontendBlock) {
        let id = block.id ?? UUID().uuidString
        let kind: TranscriptEntry.Kind = block.tone == "error" ? .error : .event
        if let index = transcript.firstIndex(where: { $0.id == id }) {
            transcript[index].text = block.append ? transcript[index].text + block.text : block.text
            transcript[index].kind = kind
            if block.group != nil { transcript[index].group = block.group }
            transcript[index].pending = block.pending
            transcript[index].format = block.format
            transcript[index].tone = block.tone
        } else {
            transcript.append(TranscriptEntry(
                id: id,
                text: block.append ? String(block.text.drop(while: { $0 == "\n" })) : block.text,
                kind: kind,
                group: block.group,
                format: block.format,
                tone: block.tone,
                pending: block.pending
            ))
        }
        if block.format == "unified_diff", !block.pending {
            refreshArtifacts()
            refreshGitDiff()
        }
    }

    private func apply(_ preview: RenderedPreview, selection: FrontendPickerOption?) {
        var blocks: [FrontendBlock] = []
        for rendered in preview.events {
            blocks.append(contentsOf: rendered.blocks)
            if let block = renderedBlock(from: rendered.event) {
                blocks.append(block)
            } else if rendered.blocks.isEmpty {
                blocks.append(contentsOf: previewBlocks(for: rendered))
            }
        }
        blocks = reducePreviewBlocks(blocks)
        guard !blocks.isEmpty else { return }
        let id = "preview-\(preview.title)"
        let existing = previews.first { $0.id == id }
        let record = TranscriptPreview(
            id: id,
            title: preview.title,
            status: selection?.description ?? existing?.status,
            model: selection?.detail ?? existing?.model,
            blocks: blocks.enumerated().map { index, block in
                PreviewBlock(id: block.id ?? "\(id)-\(index)", block: block)
            }
        )
        if let index = previews.firstIndex(where: { $0.id == id }) {
            previews[index] = record
        } else {
            previews.append(record)
        }
        if selection != nil { presentedPreview = record }
    }

    private func reducePreviewBlocks(_ blocks: [FrontendBlock]) -> [FrontendBlock] {
        var result: [FrontendBlock] = []
        for block in blocks {
            guard let id = block.id,
                  let index = result.lastIndex(where: { $0.id == id })
            else {
                result.append(block)
                continue
            }
            let current = result[index]
            result[index] = FrontendBlock(
                id: id,
                group: block.group ?? current.group,
                append: false,
                pending: block.pending,
                text: block.append ? current.text + block.text : block.text,
                format: block.format,
                tone: block.tone
            )
        }
        return result
    }

    private func previewBlocks(for rendered: RenderedEventRecord) -> [FrontendBlock] {
        let type = rendered.event["type"]?.stringValue
        let tone: String
        let text: String?
        switch type {
        case "web_search_begin":
            tone = "warning"
            text = "Searching the web"
        case "web_search_end":
            tone = "success"
            text = "Searched: \(rendered.event["query"]?.stringValue ?? rendered.event["action"]?.stringValue ?? "complete")"
        case "turn_aborted":
            tone = "warning"
            text = "Turn aborted: \(rendered.event["reason"]?.stringValue ?? "Unknown reason")"
        case "warning":
            tone = "warning"
            text = rendered.event["message"]?.stringValue
        case "error":
            tone = "error"
            text = rendered.event["message"]?.stringValue
        default:
            tone = "neutral"
            text = rendered.previewText.first
        }
        guard let text, !text.isEmpty else { return [] }
        return [FrontendBlock(
            id: nil,
            group: nil,
            append: false,
            pending: false,
            text: text,
            format: "plain_text",
            tone: tone
        )]
    }

    private func appendText(
        _ text: String?,
        kind: TranscriptEntry.Kind,
        tone: String = "neutral"
    ) {
        guard let text, !text.isEmpty else { return }
        transcript.append(TranscriptEntry(
            id: UUID().uuidString,
            text: text,
            kind: kind,
            format: "plain_text",
            tone: tone,
            pending: false
        ))
    }

    private func appendStream(id: String, delta: String, kind: TranscriptEntry.Kind) {
        guard !delta.isEmpty else { return }
        if let index = transcript.lastIndex(where: { $0.id == id }) {
            transcript[index].text.append(delta)
        } else {
            transcript.append(TranscriptEntry(
                id: id,
                text: delta,
                kind: kind,
                format: "plain_text",
                tone: "neutral",
                pending: true
            ))
        }
    }

    private func completeStream(text: String, kind: TranscriptEntry.Kind) {
        if let index = transcript.lastIndex(where: { $0.pending && $0.kind == kind }) {
            transcript[index].text = text
            transcript[index].pending = false
        } else {
            appendText(text, kind: kind)
        }
    }

    private func finishPendingTranscriptEntries() {
        for entry in transcript where entry.pending {
            entry.pending = false
        }
    }

    private func setPairingCode(_ code: String, expiresAt: Date) {
        pairingCodeExpiryTask?.cancel()
        guard expiresAt > .now else {
            pairingCodeInfo = nil
            pairingCodeExpiryTask = nil
            return
        }
        pairingCodeInfo = PairingCodeInfo(code: code, expiresAt: expiresAt)
        pairingCodeExpiryTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(max(0, expiresAt.timeIntervalSinceNow)))
            guard !Task.isCancelled,
                  let self,
                  self.pairingCodeInfo?.expiresAt == expiresAt
            else { return }
            self.pairingCodeInfo = nil
            self.pairingCodeExpiryTask = nil
        }
    }

    private func decodeApproval(_ value: JSONValue) -> PendingApproval? {
        guard let id = value["id"]?.stringValue else { return nil }
        let calls = value["calls"]?.arrayValue?.compactMap { call -> ApprovalCall? in
            guard let callID = call["callId"]?.stringValue,
                  let name = call["name"]?.stringValue
            else { return nil }
            return ApprovalCall(
                id: callID,
                name: name,
                arguments: call["arguments"]?.prettyPrinted ?? "{}"
            )
        } ?? []
        return PendingApproval(
            id: id,
            reason: value["reason"]?.stringValue ?? "Horus needs permission to continue.",
            calls: calls
        )
    }

    private func widgets(in slot: String) -> [MountedWidget] {
        mountedWidgets.filter { $0.widget.slot == slot }
    }

    private func requestID(_ prefix: String) -> String {
        "\(prefix)-\(UUID().uuidString.lowercased())"
    }

    private func restoreDraft(id: String) {
        guard let draft = pendingDrafts.removeValue(forKey: id) else { return }
        restoreDraft(draft)
    }

    private func restoreDraft(_ draft: String) {
        composer = composer.isEmpty ? draft : "\(draft)\n\n\(composer)"
    }

    private func restorePendingDrafts() {
        let drafts = pendingDrafts.keys.sorted().compactMap { pendingDrafts[$0] }
        pendingDrafts.removeAll()
        guard !drafts.isEmpty else { return }
        restoreDraft(drafts.joined(separator: "\n\n"))
    }

    private func connectionEnded(generation: UUID, message: String) {
        guard connectionGeneration == generation else { return }
        restorePendingDrafts()
        connectionState = .failed(message)
        if pendingPairingAccount != nil { pairingError = message }
        showToast(message, tone: .error)
    }

    @discardableResult
    private func resetGatewayState(
        preservingDrafts: Bool,
        preservingSession: Bool = false
    ) -> UUID {
        connectionGeneration = UUID()
        eventTask?.cancel()
        eventTask = nil
        if !preservingSession { latestSequence = nil }
        sessionOpenCursor = nil
        if preservingDrafts {
            restorePendingDrafts()
        } else {
            pendingDrafts.removeAll()
            composer = ""
        }
        pendingPairingAccount = nil
        connectionState = .disconnected
        dismissToast()
        sessionRequestID = nil
        sessionMutationRequestID = nil
        sessionToRestoreID = nil
        configRequestID = nil
        defaultConfigRequestID = nil
        applyState = .idle
        workspaceError = nil
        isChangingWorkspace = false
        showsWorkspaceBrowser = false
        directoryListing = nil
        directoryError = nil
        directoryRequestID = nil
        isLoadingDirectories = false
        if !preservingSession {
            sessions = []
            selectedSessionID = nil
            unreadSessionIDs.removeAll()
            profile = nil
            modelChoices = []
            middlewareFeatures = []
            providerStatuses = []
            defaultAgentSnapshot = nil
        }
        providerAPIKey = ""
        providerActionState = .idle
        credentialRequestID = nil
        providerLoginRequestID = nil
        providerRegistrationRequestID = nil
        providerRegistrationTarget = nil
        pairingCodeRequestID = nil
        pairingCodeExpiryTask?.cancel()
        pairingCodeExpiryTask = nil
        pairingCodeInfo = nil
        pairingCode = ""
        pairingError = nil
        if !preservingSession { resetSessionState() }
        return connectionGeneration
    }

    private func resetSessionState() {
        workspace = nil
        gitStatus = nil
        gitDiff = ""
        gitDiffRequestID = nil
        gitBranchRequestID = nil
        selectedModelRoute = ""
        contributions = []
        toolCount = 0
        agentSnapshot = nil
        agentDraft = nil
        applyState = .idle
        configRequestID = nil
        cronTasks = []
        cronRuns = []
        cronTaskDraft = ""
        cronError = nil
        cronRequestIDs.removeAll()
        transcript = []
        activeTurnID = nil
        activeOperation = nil
        steeringQueued = false
        steeringSubmissionID = nil
        contextTokens = 0
        modelContextWindow = nil
        pendingApproval = nil
        approvalRequestID = nil
        pendingPicker = nil
        mountedWidgets = []
        artifacts = []
        previews = []
        presentedPreview = nil
        previewSelections.removeAll()
        selectedArtifactID = nil
        showsInspector = false
        currentUsage = TokenUsage()
        lastUsage = TokenUsage()
    }
}

private extension TokenUsage {
    init?(json: JSONValue) {
        guard let inputTokens = json["inputTokens"]?.intValue,
              let cachedInputTokens = json["cachedInputTokens"]?.intValue,
              let cacheWriteInputTokens = json["cacheWriteInputTokens"]?.intValue,
              let outputTokens = json["outputTokens"]?.intValue,
              let reasoningOutputTokens = json["reasoningOutputTokens"]?.intValue,
              let totalTokens = json["totalTokens"]?.intValue
        else { return nil }
        self.inputTokens = inputTokens
        self.cachedInputTokens = cachedInputTokens
        self.cacheWriteInputTokens = cacheWriteInputTokens
        self.outputTokens = outputTokens
        self.reasoningOutputTokens = reasoningOutputTokens
        self.totalTokens = totalTokens
    }
}

private extension JSONValue {
    var prettyPrinted: String {
        guard let data = try? JSONEncoder().encode(self),
              let object = try? JSONSerialization.jsonObject(with: data),
              let pretty = try? JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys]),
              let text = String(data: pretty, encoding: .utf8)
        else { return "{}" }
        return text
    }
}
