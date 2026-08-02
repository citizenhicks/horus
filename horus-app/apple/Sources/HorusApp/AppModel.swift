import Foundation
import Observation

enum AppDestination: Equatable {
    case chat
    case gateway
    case agent
    case providers
    case cron
    case profile

    var symbol: String {
        switch self {
        case .chat: "messages-square"
        case .gateway: "server"
        case .agent: "sliders-horizontal"
        case .providers: "network"
        case .cron: "calendar-clock"
        case .profile: "settings"
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

enum ThemePreference: String, CaseIterable, Identifiable {
    case system
    case dark
    case light

    var id: Self { self }
}

enum InspectorSection: String, CaseIterable, Identifiable {
    case diff = "Diff"
    case subagents = "Subagents"

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
    var format: String
    var pending: Bool

    init(id: String, text: String, kind: Kind, format: String, pending: Bool) {
        self.id = id
        self.text = text
        self.kind = kind
        self.format = format
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
    var workspace: WorkspaceSummary?
    var gitStatus: GitStatus?
    var gitDiff = ""
    var sessions: [SessionSummary] = []
    var selectedSessionID: String?
    var transcript: [TranscriptEntry] = []
    var composer = ""
    var errorMessage: String?
    var activeTurnID: String?
    var activeOperation: String?
    var turnStartedAt: Date?
    var completedGenerationTime: TimeInterval = 0
    var steeringQueued = false
    var contextTokens = 0
    var modelContextWindow: Int64?
    var pendingApproval: PendingApproval?
    var modelChoices: [ModelChoice] = []
    var selectedModelRoute = ""
    var contributions: [FrontendContribution] = []
    var mountedWidgets: [MountedWidget] = []
    var pendingPicker: FrontendPickerPrompt?
    var artifacts: [ArtifactRecord] = []
    var selectedArtifactID: String?
    var showsInspector = false
    var inspectorSection = InspectorSection.diff
    var inspectorPickerOptions: [FrontendPickerOption] = []
    var profile: ProfileSnapshot?
    var currentUsage = TokenUsage()
    var cronTasks: [CronTask] = []
    var cronRuns: [CronRun] = []
    var cronTaskDraft = ""
    var cronScheduleDraft = ""
    var cronError: String?
    var workspaceError: String?
    var isChangingWorkspace = false
    var showsWorkspaceBrowser = false
    var directoryListing: DirectoryListing?
    var directoryError: String?
    var isLoadingDirectories = false
    var showsCronTaskBrowser = false
    var cronDirectoryListing: DirectoryListing?
    var cronDirectoryError: String?
    var isLoadingCronDirectories = false

    var agentSnapshot: VersionedAgentConfig?
    var agentDraft: AgentComposition?
    var applyState: ApplyState = .idle
    var providerStatuses: [ProviderStatus] = []
    var providerAPIKey = ""
    var providerActionState: ProviderActionState = .idle
    var pairingCodeInfo: PairingCodeInfo?

    var showsPairing = false
    var pairingEndpoint = "tls://"
    var pairingCode = ""
    var pairingError: String?
    var theme: ThemePreference

    @ObservationIgnored private let client: GatewayClient
    @ObservationIgnored private let store: GatewayStore
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    @ObservationIgnored private var connectionGeneration = UUID()
    @ObservationIgnored private var pendingPairingAccount: GatewayAccount?
    @ObservationIgnored private var pendingDrafts: [String: String] = [:]
    @ObservationIgnored private var sessionRequestID: String?
    @ObservationIgnored private var configRequestID: String?
    @ObservationIgnored private var approvalRequestID: String?
    @ObservationIgnored private var workspaceRequestID: String?
    @ObservationIgnored private var directoryRequestID: String?
    @ObservationIgnored private var cronDirectoryRequestID: String?
    @ObservationIgnored private var gitDiffRequestID: String?
    @ObservationIgnored private var credentialRequestID: String?
    @ObservationIgnored private var pairingCodeRequestID: String?
    @ObservationIgnored private var pairingCodeExpiryTask: Task<Void, Never>?
    @ObservationIgnored private var providerLoginRequestID: String?
    @ObservationIgnored private var cronRequestIDs: Set<String> = []
    @ObservationIgnored private var latestSequence: UInt64?
    @ObservationIgnored private var sequenceSaveTask: Task<Void, Never>?
    @ObservationIgnored private var inspectorPickerSubmissionID: String?
    @ObservationIgnored private var steeringSubmissionID: String?

    init(client: GatewayClient? = nil, store: GatewayStore? = nil) {
        let client = client ?? GatewayClient()
        let store = store ?? GatewayStore()
        self.client = client
        self.store = store
        self.accounts = store.loadAccounts()
        self.selectedAccountID = store.selectedAccountID()
        self.theme = ThemePreference(rawValue: UserDefaults.standard.string(forKey: "theme") ?? "") ?? .system
        if selectedAccountID == nil { selectedAccountID = accounts.first?.id }
        showsPairing = accounts.isEmpty
    }

    deinit {
        eventTask?.cancel()
        pairingCodeExpiryTask?.cancel()
        sequenceSaveTask?.cancel()
    }

    var selectedAccount: GatewayAccount? {
        accounts.first { $0.id == selectedAccountID }
    }

    var canOpenSession: Bool {
        connectionState.isReady
            && activeTurnID == nil
            && pendingApproval == nil
            && sessionRequestID == nil
    }

    var contextFillFraction: Double {
        guard let modelContextWindow, modelContextWindow > 0 else { return 0 }
        return min(max(Double(contextTokens) / Double(modelContextWindow), 0), 1)
    }

    var contextFillPercent: Int {
        Int((contextFillFraction * 100).rounded())
    }

    func generationElapsed(at date: Date) -> TimeInterval {
        completedGenerationTime + (turnStartedAt.map { max(0, date.timeIntervalSince($0)) } ?? 0)
    }

    var canForkSession: Bool {
        canOpenSession && contributions.contains { contribution in
            contribution.commands.contains { $0.name == "fork" }
        }
    }

    var currentSessionTitle: String {
        let session = sessions.first(where: { $0.sessionId == selectedSessionID })
        guard let message = (session?.title ?? session?.firstUserMessage)?
            .trimmingCharacters(in: .whitespacesAndNewlines),
            !message.isEmpty
        else { return "New conversation" }
        return String(message.prefix(72))
    }

    var composerHeaderWidgets: [MountedWidget] { widgets(in: "composer_header") }
    var composerFooterWidgets: [MountedWidget] { widgets(in: "composer_footer") }

    func start() {
        guard let account = selectedAccount else {
            showsPairing = true
            return
        }
        connect(to: account)
    }

    func pair() {
        pairingError = nil
        do {
            let endpoint = try GatewayEndpoint(pairingEndpoint)
            let code = pairingCode.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !code.isEmpty else {
                pairingError = "Enter the one-time code shown by the gateway."
                return
            }
            let account = accounts.first(where: { $0.endpoint == endpoint })
                ?? GatewayAccount(endpoint: endpoint)
            flushLatestSequence()
            let generation = resetGatewayState(preservingDrafts: account.id == selectedAccountID)
            pendingPairingAccount = account
            beginConnection(to: endpoint, generation: generation) { [weak self] in
                guard let self, self.connectionGeneration == generation else { return }
                try await self.client.send(.pair(
                    code: code,
                    clientLabel: "Horus Apple",
                    lastSequence: nil
                ))
            }
        } catch {
            pairingError = error.localizedDescription
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
        guard path != workspace?.label else { return }
        let id = requestID("workspace")
        workspaceRequestID = id
        workspaceError = nil
        isChangingWorkspace = true
        transmit(.setWorkspace(requestID: id, path: path)) { [weak self] message in
            self?.workspaceRequestID = nil
            self?.isChangingWorkspace = false
            self?.workspaceError = message
        }
    }

    func openWorkspaceBrowser() {
        guard connectionState.isReady, let path = workspace?.label else { return }
        showsWorkspaceBrowser = true
        loadDirectory(path)
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

    func openCronTaskBrowser() {
        guard connectionState.isReady, let path = workspace?.label else { return }
        showsCronTaskBrowser = true
        loadCronDirectory(path)
    }

    func loadCronDirectory(_ path: String) {
        let id = requestID("cron-directories")
        cronDirectoryRequestID = id
        cronDirectoryError = nil
        isLoadingCronDirectories = true
        transmit(.listDirectories(requestID: id, path: path, includeFiles: true)) { [weak self] message in
            guard self?.cronDirectoryRequestID == id else { return }
            self?.cronDirectoryRequestID = nil
            self?.isLoadingCronDirectories = false
            self?.cronDirectoryError = message
        }
    }

    func chooseCronTask(_ path: String) {
        cronTaskDraft = path
        cronError = nil
        showsCronTaskBrowser = false
    }

    func forgetSelectedGateway() {
        guard let account = selectedAccount else { return }
        do {
            flushLatestSequence()
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
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func openNewSession() {
        openSession(nil)
    }

    func openSession(_ sessionID: String?) {
        guard canOpenSession else { return }
        let id = requestID("open")
        sessionRequestID = id
        transmit(.openSession(requestID: id, sessionID: sessionID)) { [weak self] message in
            guard self?.sessionRequestID == id else { return }
            self?.sessionRequestID = nil
            self?.errorMessage = message
        }
    }

    func renameSession(_ session: SessionSummary, title: String) {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return }
        transmit(.renameSession(
            requestID: requestID("session-rename"),
            sessionID: session.sessionId,
            title: title
        ))
    }

    func setSessionPinned(_ session: SessionSummary, pinned: Bool) {
        transmit(.setSessionPinned(
            requestID: requestID("session-pin"),
            sessionID: session.sessionId,
            pinned: pinned
        ))
    }

    func deleteSession(_ session: SessionSummary) {
        guard session.sessionId != selectedSessionID else { return }
        transmit(.deleteSession(
            requestID: requestID("session-delete"),
            sessionID: session.sessionId
        ))
    }

    func selectGitBranch(_ branch: String) {
        guard branch != gitStatus?.currentBranch else { return }
        transmit(.setGitBranch(requestID: requestID("git-branch"), branch: branch))
    }

    func refreshGitDiff() {
        guard connectionState.isReady else { return }
        let id = requestID("git-diff")
        gitDiffRequestID = id
        transmit(.getGitDiff(requestID: id))
    }

    func sendMessage() {
        let text = composer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        guard text.utf8.count <= maximumComposerBytes else {
            errorMessage = "Messages are limited to 1 MiB."
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
        transmit(.submit(Submission(id: id, op: op))) { [weak self] message in
            self?.restoreDraft(id: id)
            if self?.steeringSubmissionID == id {
                self?.steeringSubmissionID = nil
                self?.steeringQueued = false
            }
            self?.errorMessage = message
        }
    }

    func submitWidget(_ mounted: MountedWidget, presentsPickerInInspector: Bool = false) {
        guard let action = mounted.widget.action else { return }
        let id = requestID("widget")
        if presentsPickerInInspector {
            pendingPicker = nil
            inspectorPickerSubmissionID = id
            inspectorSection = .subagents
        }
        transmit(.submit(Submission(id: id, op: action))) { [weak self] message in
            if self?.inspectorPickerSubmissionID == id { self?.inspectorPickerSubmissionID = nil }
            self?.errorMessage = message
        }
    }

    func submitPickerOption(_ option: FrontendPickerOption) {
        pendingPicker = nil
        transmit(.submit(Submission(id: requestID("picker"), op: option.op)))
    }

    func selectModel(_ route: String) {
        guard route != selectedModelRoute else { return }
        transmit(.submit(Submission(id: requestID("model"), op: .setModel(route: route))))
    }

    func interrupt() {
        guard let activeTurnID else { return }
        transmit(.submit(Submission(
            id: requestID("interrupt"),
            op: .interrupt(turnID: activeTurnID)
        )))
    }

    func resolveApproval(_ decision: ReviewDecision) {
        guard let approval = pendingApproval, approvalRequestID == nil else { return }
        let id = requestID("approval")
        approvalRequestID = id
        transmit(.submit(Submission(
            id: id,
            op: .execApproval(id: approval.id, decision: decision)
        ))) { [weak self] message in
            guard self?.approvalRequestID == id else { return }
            self?.approvalRequestID = nil
            self?.errorMessage = message
        }
    }

    func selectArtifact(_ id: String) {
        selectedArtifactID = id
        inspectorSection = artifacts.first(where: { $0.id == id })?.kind == "code_diff" ? .diff : .subagents
        showsInspector = true
        if inspectorSection == .diff { refreshGitDiff() }
    }

    func showInspector(_ section: InspectorSection) {
        inspectorSection = section
        showsInspector = true
        if section == .diff { refreshGitDiff() }
    }

    func toggleInspector() {
        if showsInspector {
            showsInspector = false
        } else {
            showInspector(inspectorSection)
        }
    }

    func openInspectorPickerOption(_ option: FrontendPickerOption) {
        transmit(.submit(Submission(id: requestID("picker"), op: option.op)))
    }

    func forkSession() {
        guard canOpenSession,
              let contribution = contributions.first(where: { contribution in
                  contribution.commands.contains { $0.name == "fork" }
              })
        else { return }
        transmit(.submit(Submission(
            id: requestID("fork"),
            op: .capabilityCommand(capability: contribution.capability, command: "fork", arguments: "")
        )))
    }

    func applyAgentConfiguration() {
        guard let snapshot = agentSnapshot, let draft = agentDraft else { return }
        let id = requestID("configure")
        configRequestID = id
        applyState = .applying
        transmit(.configureAgent(
            requestID: id,
            expectedRevision: snapshot.revision,
            config: draft
        )) { [weak self] message in
            self?.applyState = .failed(message)
        }
    }

    func setApprovalPolicy(_ policy: ApprovalPolicy) {
        guard let snapshot = agentSnapshot, let draft = agentDraft else { return }
        guard draft == snapshot.config else {
            errorMessage = "Apply or reload pending agent/provider edits before changing approval."
            return
        }
        guard draft.approval != policy else { return }
        agentDraft?.approval = policy
        applyAgentConfiguration()
    }

    func reloadAgentDraft() {
        agentDraft = agentSnapshot?.config
        applyState = .idle
    }

    func selectProvider(_ provider: String) {
        guard let status = providerStatuses.first(where: { $0.provider == provider }) else { return }
        agentDraft?.provider = ProviderSelection(
            provider: status.provider,
            model: status.defaultModel ?? "",
            baseUrl: status.defaultBaseUrl,
            apiKeyEnv: nil,
            reasoningEffort: status.defaultReasoningEffort,
            webSearch: status.defaultWebSearch
        )
        providerAPIKey = ""
        providerActionState = .idle
    }

    func saveProviderCredential(provider: String) {
        let key = providerAPIKey
        guard !key.isEmpty else {
            providerActionState = .failed("Enter an API key. It will be sent once and never read back.")
            return
        }
        let id = requestID("credential")
        credentialRequestID = id
        providerActionState = .savingCredential(provider)
        transmit(.setProviderCredential(requestID: id, provider: provider, apiKey: key)) { [weak self] message in
            self?.providerActionState = .failed(message)
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
        transmit(.createPairingCode(requestID: id)) { [weak self] message in
            self?.errorMessage = message
        }
    }

    func addCron() {
        let task = cronTaskDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        let schedule = cronScheduleDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !task.isEmpty, !schedule.isEmpty else {
            cronError = "Choose a task file and enter a schedule."
            return
        }
        let id = requestID("cron-add")
        cronRequestIDs.insert(id)
        cronError = nil
        transmit(.addCron(requestID: id, task: task, schedule: schedule)) { [weak self] message in
            self?.cronRequestIDs.remove(id)
            self?.cronError = message
        }
    }

    func rescheduleCron(_ task: CronTask, schedule: String) {
        let value = schedule.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        let request = requestID("cron-reschedule")
        cronRequestIDs.insert(request)
        transmit(.rescheduleCron(requestID: request, id: task.id, schedule: value)) { [weak self] message in
            self?.cronRequestIDs.remove(request)
            self?.cronError = message
        }
    }

    func deleteCron(_ task: CronTask) {
        let request = requestID("cron-delete")
        cronRequestIDs.insert(request)
        transmit(.deleteCron(requestID: request, id: task.id)) { [weak self] message in
            self?.cronRequestIDs.remove(request)
            self?.cronError = message
        }
    }

    func runCron(_ task: CronTask) {
        let request = requestID("cron-run")
        cronRequestIDs.insert(request)
        transmit(.runCron(requestID: request, id: task.id)) { [weak self] message in
            self?.cronRequestIDs.remove(request)
            self?.cronError = message
        }
    }

    func refreshCron() {
        transmit(.listCron(requestID: requestID("cron-list")))
        transmit(.listCronHistory(requestID: requestID("cron-history"), id: nil))
    }

    func setTheme(_ theme: ThemePreference) {
        self.theme = theme
        UserDefaults.standard.set(theme.rawValue, forKey: "theme")
    }

    private func connect(to account: GatewayAccount) {
        flushLatestSequence()
        let generation = resetGatewayState(preservingDrafts: account.id == selectedAccountID)
        selectedAccountID = account.id
        store.select(account)
        let lastSequence = store.lastSequence(for: account)
        latestSequence = lastSequence
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
                        lastSequence: lastSequence
                    ))
                }
            } catch {
                self.connectionState = .failed(error.localizedDescription)
                self.errorMessage = error.localizedDescription
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
        errorMessage = nil

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
                try await self.client.send(request)
            } catch {
                guard generation == self.connectionGeneration else { return }
                let message = error.localizedDescription
                if let onFailure { onFailure(message) }
                else { self.errorMessage = message }
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
            } catch {
                pairingError = error.localizedDescription
            }
        case .authenticated:
            connectionState = .loading
        case .ready(let payload):
            applyReady(payload)
        case .accepted(let requestID):
            handleAccepted(requestID)
        case .rejected(let rejection):
            handleRejected(rejection)
        case .agentEvent(let sequence, let event, let blocks, let history, let preview):
            guard latestSequence.map({ sequence > $0 }) ?? true else { return }
            latestSequence = sequence
            if let account = selectedAccount { scheduleSequenceSave(sequence, for: account) }
            reduce(event: event, blocks: blocks, history: history, preview: preview)
        case .sessions(let sessions):
            self.sessions = sessions.filter(\.catalogVisible)
        case .configChanged(let snapshot):
            agentSnapshot = snapshot
            agentDraft = snapshot.config
            if applyState == .applying || applyState == .restarting { applyState = .restarting }
        case .providerCredentialStatus(let requestID, let provider, let configured):
            if let index = providerStatuses.firstIndex(where: { $0.provider == provider }) {
                providerStatuses[index].configured = configured
            }
            if requestID == credentialRequestID {
                credentialRequestID = nil
                if configured {
                    providerAPIKey = ""
                    providerActionState = .credentialSaved(provider)
                } else {
                    providerActionState = .failed("The gateway did not store the provider credential.")
                }
            }
        case .pairingCode(let requestID, let code, let expiresAt):
            guard requestID == pairingCodeRequestID else { break }
            pairingCodeRequestID = nil
            setPairingCode(
                code,
                expiresAt: Date(timeIntervalSince1970: TimeInterval(expiresAt))
            )
        case .providerLoginStarted(_, _, let provider, let url, let code):
            providerActionState = .deviceCode(
                provider: provider,
                url: url,
                code: code
            )
        case .providerLoginFinished(_, _, let provider):
            providerLoginRequestID = nil
            providerActionState = .loginFinished(provider)
            if let index = providerStatuses.firstIndex(where: { $0.provider == provider }) {
                providerStatuses[index].configured = true
            }
        case .profile(_, let profile):
            self.profile = profile
        case .artifacts(_, let artifacts):
            let remoteIDs = Set(artifacts.map(\.id))
            let previews = self.artifacts.filter { $0.kind == "preview" && !remoteIDs.contains($0.id) }
            self.artifacts = artifacts + previews
            if selectedArtifactID == nil || !self.artifacts.contains(where: { $0.id == selectedArtifactID }) {
                selectedArtifactID = self.artifacts.first?.id
            }
        case .gitDiff(let requestID, let diff):
            guard requestID == gitDiffRequestID else { break }
            gitDiffRequestID = nil
            gitDiff = diff
        case .directories(let requestID, let listing):
            if requestID == directoryRequestID {
                directoryRequestID = nil
                directoryListing = listing
                directoryError = nil
                isLoadingDirectories = false
            } else if requestID == cronDirectoryRequestID {
                cronDirectoryRequestID = nil
                cronDirectoryListing = listing
                cronDirectoryError = nil
                isLoadingCronDirectories = false
            }
        case .cronTasks(let requestID, let tasks):
            cronRequestIDs.remove(requestID)
            cronTasks = tasks
        case .cronHistory(let requestID, let runs):
            cronRequestIDs.remove(requestID)
            cronRuns = runs
        case .error(let failure):
            if failure.code == "replay_unavailable" {
                sequenceSaveTask?.cancel()
                sequenceSaveTask = nil
                resetSessionState()
                latestSequence = nil
                if let account = selectedAccount { store.clearLastSequence(for: account) }
            }
            if pendingPairingAccount != nil { pairingError = failure.message }
            if failure.code == "unauthorized", pendingPairingAccount == nil {
                repairSelectedGateway()
            }
            errorMessage = failure.message
            if failure.fatal {
                restorePendingDrafts()
                connectionState = .failed(failure.message)
            }
        case .unknown:
            break
        }
    }

    private func applyReady(_ payload: ReadyPayload) {
        sessionRequestID = nil
        if selectedSessionID != payload.session.sessionId {
            restorePendingDrafts()
            resetSessionState()
        }
        let workspaceChanged = workspaceRequestID != nil
        workspace = payload.workspace
        gitStatus = payload.git
        if workspaceChanged {
            workspaceRequestID = nil
            workspaceError = nil
            isChangingWorkspace = false
            showsWorkspaceBrowser = false
        }
        sessions = payload.sessions.filter(\.catalogVisible)
        selectedSessionID = payload.session.sessionId
        selectedModelRoute = payload.session.model.route
        modelContextWindow = payload.session.model.modelContextWindow
        modelChoices = payload.modelChoices
        contributions = payload.contributions
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
        providerStatuses = payload.providers
        connectionState = .ready
        errorMessage = nil
        if applyState == .restarting { applyState = .applied }
        requestWorkspaceData()
    }

    private func requestWorkspaceData() {
        transmit(.getProfile(requestID: requestID("profile")))
        transmit(.listArtifacts(requestID: requestID("artifacts")))
        refreshGitDiff()
        refreshCron()
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
        if cronRequestIDs.remove(requestID) != nil {
            cronTaskDraft = ""
            cronScheduleDraft = ""
            refreshCron()
        }
    }

    private func handleRejected(_ rejection: GatewayRejection) {
        if pendingDrafts[rejection.requestId] != nil {
            restoreDraft(id: rejection.requestId)
        }
        if rejection.requestId == configRequestID {
            switch rejection.code {
            case "revision_conflict": applyState = .conflict(rejection.message)
            case "agent_busy": applyState = .busy(rejection.message)
            case "invalid_config": applyState = .invalid(rejection.message)
            default: applyState = .failed(rejection.message)
            }
            configRequestID = nil
        }
        if rejection.requestId == approvalRequestID {
            approvalRequestID = nil
        }
        if rejection.requestId == sessionRequestID {
            sessionRequestID = nil
            errorMessage = rejection.message
        }
        if rejection.requestId == workspaceRequestID {
            workspaceError = rejection.message
            workspaceRequestID = nil
            isChangingWorkspace = false
        }
        if rejection.requestId == directoryRequestID {
            directoryError = rejection.message
            directoryRequestID = nil
            isLoadingDirectories = false
        }
        if rejection.requestId == cronDirectoryRequestID {
            cronDirectoryError = rejection.message
            cronDirectoryRequestID = nil
            isLoadingCronDirectories = false
        }
        if rejection.requestId == gitDiffRequestID {
            gitDiffRequestID = nil
        }
        if rejection.requestId == inspectorPickerSubmissionID {
            inspectorPickerSubmissionID = nil
        }
        if rejection.requestId == credentialRequestID {
            providerActionState = .failed(rejection.message)
            credentialRequestID = nil
        }
        if rejection.requestId == providerLoginRequestID {
            providerActionState = .failed(rejection.message)
            providerLoginRequestID = nil
        }
        if rejection.requestId == pairingCodeRequestID {
            errorMessage = rejection.message
            pairingCodeRequestID = nil
        }
        if cronRequestIDs.remove(rejection.requestId) != nil {
            cronError = rejection.message
        }
        errorMessage = rejection.message
        if rejection.fatal {
            restorePendingDrafts()
            connectionState = .failed(rejection.message)
        }
    }

    private func reduce(
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
            let text = preview.events
                .flatMap(\.previewText)
                .joined(separator: "\n\n")
            if !text.isEmpty {
                upsertArtifact(ArtifactRecord(
                    id: "preview-\(preview.title)",
                    sessionId: selectedSessionID ?? "",
                    kind: "preview",
                    title: preview.title,
                    block: FrontendBlock(
                        id: nil,
                        group: nil,
                        append: false,
                        pending: false,
                        text: text,
                        format: "plain_text",
                        tone: "neutral"
                    )
                ))
                showsInspector = true
            }
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
            turnStartedAt = .now
            if let window = event.msg["modelContextWindow"]?.intValue {
                modelContextWindow = Int64(window)
            }
        case "task_complete":
            finishPendingTranscriptEntries()
            flushLatestSequence()
            activeTurnID = nil
            finishGeneration()
            refreshGitDiff()
            steeringQueued = false
            steeringSubmissionID = nil
            pendingApproval = nil
            approvalRequestID = nil
        case "turn_aborted":
            finishPendingTranscriptEntries()
            flushLatestSequence()
            activeTurnID = nil
            finishGeneration()
            refreshGitDiff()
            steeringQueued = false
            steeringSubmissionID = nil
            pendingApproval = nil
            approvalRequestID = nil
        case "warning":
            appendText(event.msg["message"]?.stringValue, kind: .event)
        case "error":
            let message = event.msg["message"]?.stringValue ?? "Agent error"
            appendText(message, kind: .error)
            errorMessage = message
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
            if let usage = event.msg["info"]?["totalTokenUsage"] {
                currentUsage = TokenUsage(json: usage)
            }
            if let usage = event.msg["info"]?["lastTokenUsage"] {
                let latest = TokenUsage(json: usage)
                contextTokens = max(0, latest.inputTokens + latest.outputTokens)
            }
            if let window = event.msg["info"]?["modelContextWindow"]?.intValue {
                modelContextWindow = Int64(window)
            }
        case "frontend":
            applyFrontendEvent(event.msg, submissionID: event.submissionId, wasRendered: wasRendered)
        default:
            break
        }
    }

    private func applyFrontendEvent(_ event: JSONValue, submissionID: String?, wasRendered: Bool) {
        switch event["frontendType"]?.stringValue {
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
            if submissionID == inspectorPickerSubmissionID {
                inspectorPickerSubmissionID = nil
                inspectorPickerOptions = options
                inspectorSection = .subagents
                showsInspector = true
            } else {
                pendingPicker = FrontendPickerPrompt(title: title, options: options)
            }
        default:
            if wasRendered, submissionID == inspectorPickerSubmissionID {
                inspectorPickerSubmissionID = nil
            }
            break
        }
    }

    private func apply(_ block: FrontendBlock) {
        let id = block.id ?? UUID().uuidString
        let kind: TranscriptEntry.Kind = block.tone == "error" ? .error : .event
        if let index = transcript.firstIndex(where: { $0.id == id }) {
            transcript[index].text = block.append ? transcript[index].text + block.text : block.text
            transcript[index].pending = block.pending
            transcript[index].format = block.format
        } else {
            transcript.append(TranscriptEntry(
                id: id,
                text: block.text,
                kind: kind,
                format: block.format,
                pending: block.pending
            ))
        }
        if block.format == "unified_diff" {
            upsertArtifact(ArtifactRecord(
                id: id,
                sessionId: selectedSessionID ?? "",
                kind: "code_diff",
                title: diffTitle(block.text),
                block: block
            ))
            if !block.pending { refreshGitDiff() }
        }
    }

    private func appendText(_ text: String?, kind: TranscriptEntry.Kind) {
        guard let text, !text.isEmpty else { return }
        transcript.append(TranscriptEntry(
            id: UUID().uuidString,
            text: text,
            kind: kind,
            format: "plain_text",
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

    private func scheduleSequenceSave(_ sequence: UInt64, for account: GatewayAccount) {
        sequenceSaveTask?.cancel()
        sequenceSaveTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled, let self else { return }
            self.store.saveLastSequence(sequence, for: account)
            self.sequenceSaveTask = nil
        }
    }

    private func flushLatestSequence() {
        sequenceSaveTask?.cancel()
        sequenceSaveTask = nil
        guard let latestSequence, let account = selectedAccount else { return }
        store.saveLastSequence(latestSequence, for: account)
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

    private func upsertArtifact(_ artifact: ArtifactRecord) {
        if let index = artifacts.firstIndex(where: { $0.id == artifact.id }) {
            artifacts[index] = artifact
        } else {
            artifacts.append(artifact)
        }
        selectedArtifactID = artifact.id
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

    private func finishGeneration() {
        guard let turnStartedAt else { return }
        completedGenerationTime += max(0, Date.now.timeIntervalSince(turnStartedAt))
        self.turnStartedAt = nil
    }

    private func connectionEnded(generation: UUID, message: String) {
        guard connectionGeneration == generation else { return }
        flushLatestSequence()
        restorePendingDrafts()
        connectionState = .failed(message)
        if pendingPairingAccount != nil { pairingError = message }
        errorMessage = message
        activeTurnID = nil
        finishGeneration()
        steeringQueued = false
        steeringSubmissionID = nil
        pendingApproval = nil
        approvalRequestID = nil
    }

    @discardableResult
    private func resetGatewayState(preservingDrafts: Bool) -> UUID {
        connectionGeneration = UUID()
        eventTask?.cancel()
        eventTask = nil
        sequenceSaveTask?.cancel()
        sequenceSaveTask = nil
        latestSequence = nil
        if preservingDrafts {
            restorePendingDrafts()
        } else {
            pendingDrafts.removeAll()
            composer = ""
        }
        pendingPairingAccount = nil
        connectionState = .disconnected
        errorMessage = nil
        workspace = nil
        gitStatus = nil
        gitDiff = ""
        gitDiffRequestID = nil
        sessionRequestID = nil
        configRequestID = nil
        workspaceError = nil
        workspaceRequestID = nil
        isChangingWorkspace = false
        showsWorkspaceBrowser = false
        directoryListing = nil
        directoryError = nil
        directoryRequestID = nil
        isLoadingDirectories = false
        showsCronTaskBrowser = false
        cronDirectoryListing = nil
        cronDirectoryError = nil
        cronDirectoryRequestID = nil
        isLoadingCronDirectories = false
        sessions = []
        selectedSessionID = nil
        profile = nil
        cronTasks = []
        cronRuns = []
        cronTaskDraft = ""
        cronScheduleDraft = ""
        cronError = nil
        cronRequestIDs.removeAll()
        modelChoices = []
        selectedModelRoute = ""
        contributions = []
        agentSnapshot = nil
        agentDraft = nil
        applyState = .idle
        providerStatuses = []
        providerAPIKey = ""
        providerActionState = .idle
        credentialRequestID = nil
        providerLoginRequestID = nil
        pairingCodeRequestID = nil
        pairingCodeExpiryTask?.cancel()
        pairingCodeExpiryTask = nil
        pairingCodeInfo = nil
        pairingCode = ""
        pairingError = nil
        resetSessionState()
        return connectionGeneration
    }

    private func resetSessionState() {
        transcript = []
        activeTurnID = nil
        activeOperation = nil
        turnStartedAt = nil
        completedGenerationTime = 0
        steeringQueued = false
        steeringSubmissionID = nil
        contextTokens = 0
        modelContextWindow = nil
        pendingApproval = nil
        approvalRequestID = nil
        pendingPicker = nil
        mountedWidgets = []
        artifacts = []
        selectedArtifactID = nil
        showsInspector = false
        inspectorSection = .diff
        inspectorPickerOptions = []
        inspectorPickerSubmissionID = nil
        currentUsage = TokenUsage()
    }
}

private extension TokenUsage {
    init(json: JSONValue) {
        inputTokens = json["inputTokens"]?.intValue ?? 0
        cachedInputTokens = json["cachedInputTokens"]?.intValue ?? 0
        cacheWriteInputTokens = json["cacheWriteInputTokens"]?.intValue ?? 0
        outputTokens = json["outputTokens"]?.intValue ?? 0
        reasoningOutputTokens = json["reasoningOutputTokens"]?.intValue ?? 0
        totalTokens = json["totalTokens"]?.intValue ?? 0
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

func diffTitle(_ diff: String) -> String {
    for line in diff.split(separator: "\n", omittingEmptySubsequences: false) {
        if line.hasPrefix("+++ b/") { return String(line.dropFirst(6)) }
        if line.hasPrefix("+++ ") { return String(line.dropFirst(4)) }
    }
    return "Code changes"
}
