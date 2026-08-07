import Foundation
import LocalAuthentication
import Observation
import UniformTypeIdentifiers

enum AppDestination: Equatable {
    case chat
    case gateway
    case agent
    case providers
    case cron
    case profile
    case contribution(String)

    var glyph: HorusGlyph {
        switch self {
        case .chat: .chatsCircle
        case .gateway: .cellTower
        case .agent: .slidersHorizontal
        case .providers: .plugsConnected
        case .cron: .calendarDots
        case .profile: .gear
        case .contribution: .squaresFour
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

enum ComposerAttachmentState: Equatable {
    case queued
    case uploading
    case uploaded(AttachmentRecord)
    case failed(String)
}

struct ComposerAttachment: Identifiable, Equatable {
    let id: UUID
    let name: String
    let size: Int64
    let mediaType: String
    var state: ComposerAttachmentState
}

private struct PendingComposerDraft {
    let text: String
    let attachments: [AttachmentRecord]
}

private enum AttachmentUploadRequest {
    case begin(localID: UUID)
    case append(localID: UUID, expectedNextOffset: Int64)
    case finish(localID: UUID)

    var localID: UUID {
        switch self {
        case .begin(let localID), .append(let localID, _), .finish(let localID): localID
        }
    }
}

private struct ActiveAttachmentUpload {
    let localID: UUID
    let sessionID: String
    let uploadID: String
    let maxChunkBytes: Int
}

private struct AttachmentPreviewDownload {
    let generation: UUID
    let attachment: AttachmentRecord
    let sessionID: String
    var data: Data
    var requestID: String
}

private struct WorkspaceFilePreviewDownload {
    let generation: UUID
    let file: WorkspaceFileRecord
    let sessionID: String
    var data: Data
    var requestID: String
}

private struct ImportedAttachmentData: Sendable {
    let name: String
    let mediaType: String
    let data: Data
}

private struct AttachmentPreviewFile: Sendable {
    let directory: URL
    let url: URL
}

private enum AttachmentImportError: LocalizedError {
    case notAFile
    case tooLarge
    case totalTooLarge
    case changedWhileReading

    var errorDescription: String? {
        switch self {
        case .notAFile: "Choose a regular file."
        case .tooLarge: "Attachments are limited to 25 MiB each."
        case .totalTooLarge: "Attachments in one message are limited to 100 MiB total."
        case .changedWhileReading: "The file changed while Horus was reading it. Try again."
        }
    }
}

enum ThemePreference: String, CaseIterable, Identifiable {
    case system
    case dark
    case light

    var id: Self { self }
}

enum InspectorPage {
    case changes
    case uploads
}

enum WorkspaceViewerScope: String, CaseIterable, Identifiable {
    case staged
    case unstaged
    case committed
    case all

    var id: Self { self }

    var title: String {
        switch self {
        case .staged: "Staged"
        case .unstaged: "Unstaged"
        case .committed: "Committed"
        case .all: "All"
        }
    }

    var gitScope: GitDiffScope? {
        switch self {
        case .staged: .staged
        case .unstaged: .unstaged
        case .committed: .committed
        case .all: nil
        }
    }
}

enum AppLockAuthenticationMethod: Equatable {
    case faceID
    case touchID
    case biometrics
    case unavailable

    var settingTitle: String {
        switch self {
        case .faceID: "Require Face ID"
        case .touchID: "Require Touch ID"
        case .biometrics: "Require Biometric Authentication"
        case .unavailable:
            #if os(macOS)
            "Require Touch ID"
            #else
            "Require Face ID or Touch ID"
            #endif
        }
    }

    var unlockTitle: String {
        switch self {
        case .faceID: "Unlock with Face ID"
        case .touchID: "Unlock with Touch ID"
        case .biometrics: "Unlock with Biometrics"
        case .unavailable:
            #if os(macOS)
            "Unlock with Touch ID"
            #else
            "Unlock with Face ID or Touch ID"
            #endif
        }
    }

    var glyph: HorusGlyph {
        switch self {
        case .faceID: .userFocus
        case .touchID: .fingerprint
        case .biometrics, .unavailable: .fingerprint
        }
    }

    var isAvailable: Bool { self != .unavailable }
}

@MainActor
struct AppLockAuthenticator {
    private let methodProvider: () -> AppLockAuthenticationMethod
    private let evaluator: (String) async -> Bool

    init(
        method: @escaping () -> AppLockAuthenticationMethod,
        authenticate: @escaping (String) async -> Bool
    ) {
        methodProvider = method
        evaluator = authenticate
    }

    init() {
        methodProvider = {
            let context = LAContext()
            var error: NSError?
            guard context.canEvaluatePolicy(
                .deviceOwnerAuthenticationWithBiometrics,
                error: &error
            ) else {
                return .unavailable
            }
            return switch context.biometryType {
            case .faceID: .faceID
            case .touchID: .touchID
            case .opticID: .biometrics
            case .none: .unavailable
            @unknown default: .biometrics
            }
        }
        evaluator = { reason in
            let context = LAContext()
            context.localizedCancelTitle = "Cancel"
            context.localizedFallbackTitle = ""
            var error: NSError?
            guard context.canEvaluatePolicy(
                .deviceOwnerAuthenticationWithBiometrics,
                error: &error
            ) else {
                return false
            }
            return (try? await context.evaluatePolicy(
                .deviceOwnerAuthenticationWithBiometrics,
                localizedReason: reason
            )) == true
        }
    }

    var method: AppLockAuthenticationMethod { methodProvider() }

    func authenticate(reason: String) async -> Bool {
        await evaluator(reason)
    }
}

private let appLockEnabledKey = "app-lock-enabled"
private let maximumAttachmentBytes = 25 * 1024 * 1024
private let maximumComposerAttachmentBytes: Int64 = 100 * 1024 * 1024
private let maximumPreviewBytes = 25 * 1024 * 1024

@Observable
final class TranscriptEntry: Identifiable {
    enum Kind: String, Codable {
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
    var messageTarget: MessageTarget?
    var attachments: [AttachmentRecord]

    init(
        id: String,
        text: String,
        kind: Kind,
        group: String? = nil,
        format: String,
        tone: String = "neutral",
        pending: Bool,
        messageTarget: MessageTarget? = nil,
        attachments: [AttachmentRecord] = []
    ) {
        self.id = id
        self.text = text
        self.kind = kind
        self.group = group
        self.format = format
        self.tone = tone
        self.pending = pending
        self.messageTarget = messageTarget
        self.attachments = attachments
    }
}

private struct BufferedAgentEvent {
    let sequence: UInt64
    let event: AgentEventRecord
    let blocks: [FrontendBlock]
    let history: [RenderedEventRecord]?
    let preview: RenderedPreview?
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
    var title: String { widget.content?.title ?? widget.text }
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
    private var replayPresentedTranscript: [TranscriptEntry]?
    var displayedTranscript: [TranscriptEntry] {
        replayPresentedTranscript ?? transcript
    }
    var composer = ""
    var composerAttachments: [ComposerAttachment] = []
    var uploadedAttachments: [AttachmentRecord] = []
    private(set) var isLoadingAttachments = false
    var previewURL: URL?
    private(set) var isLoadingAttachmentPreview = false
    var toast: AppToast?
    var activeTurnID: String?
    var activeOperation: String?
    var contextTokens = 0
    var modelContextWindow: Int64?
    var pendingApproval: PendingApproval?
    var modelChoices: [ModelChoice] = []
    var modelProviders: [String: String] = [:]
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
    var inspectorPage: InspectorPage = .changes
    var workspaceViewerScope: WorkspaceViewerScope = .unstaged
    var workspaceFiles: [WorkspaceFileRecord] = []
    private(set) var isLoadingGitDiff = false
    private(set) var isLoadingWorkspaceFiles = false
    var profile: ProfileSnapshot?
    var runStats = RunStats()
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
    private var setupProviderDraft: ProviderConfig?
    var applyState: ApplyState = .idle
    var providerStatuses: [ProviderStatus] = []
    var providerAPIKey = ""
    var providerModelIDsText = ""
    var providerActionState: ProviderActionState = .idle
    var pairingCodeInfo: PairingCodeInfo?

    var showsPairing = false
    var pairingEndpoint = "wss://"
    var pairingCode = ""
    var pairingError: String?
    var theme: ThemePreference
    private(set) var appLockEnabled: Bool
    private(set) var isAppLocked: Bool
    private(set) var isAppLockAuthenticating = false
    private(set) var appLockAuthenticationMethod: AppLockAuthenticationMethod
    private(set) var appLockError: String?

    @ObservationIgnored private let client: GatewayClient
    @ObservationIgnored private let store: GatewayStore
    @ObservationIgnored private let settingsDefaults: UserDefaults
    @ObservationIgnored private let appLockAuthenticator: AppLockAuthenticator
    @ObservationIgnored private let requestSender:
        @MainActor @Sendable (GatewayRequest) async throws -> Void
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    @ObservationIgnored private var deltaFlushTask: Task<Void, Never>?
    @ObservationIgnored private var bufferedDeltas:
        [(id: String, delta: String, kind: TranscriptEntry.Kind)] = []
    @ObservationIgnored private var connectionGeneration = UUID()
    @ObservationIgnored private var reconnectsOnActivation = false
    @ObservationIgnored private var pendingPairingAccount: GatewayAccount?
    @ObservationIgnored private var pendingDrafts: [String: PendingComposerDraft] = [:]
    @ObservationIgnored private var sessionRequestID: String?
    @ObservationIgnored private var sessionOpeningID: String?
    @ObservationIgnored private var pendingCachedTranscript: CachedTranscript?
    @ObservationIgnored private var pendingPresentedTranscript: [TranscriptEntry]?
    private var sessionMutationRequestID: String?
    @ObservationIgnored private var sessionToRestoreID: String?
    @ObservationIgnored private var configRequestID: String?
    @ObservationIgnored private var defaultConfigRequestID: String?
    @ObservationIgnored private var approvalRequestID: String?
    @ObservationIgnored private var directoryRequestID: String?
    @ObservationIgnored private var gitDiffRequestID: String?
    @ObservationIgnored private var workspaceFilesRequestID: String?
    @ObservationIgnored private var attachmentListRequestID: String?
    @ObservationIgnored private var attachmentUploadRequests: [String: AttachmentUploadRequest] = [:]
    @ObservationIgnored private var attachmentData: [UUID: Data] = [:]
    @ObservationIgnored private var attachmentImportReservations = 0
    @ObservationIgnored private var attachmentImportGeneration = UUID()
    @ObservationIgnored private var activeAttachmentUpload: ActiveAttachmentUpload?
    @ObservationIgnored private var attachmentPreviewDownload: AttachmentPreviewDownload?
    @ObservationIgnored private var workspaceFilePreviewDownload: WorkspaceFilePreviewDownload?
    @ObservationIgnored private var attachmentPreviewGeneration = UUID()
    @ObservationIgnored private var previewTemporaryDirectory: URL?
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
    @ObservationIgnored private var currentReplayEpoch: String?
    @ObservationIgnored private var sessionOpenCursor: UInt64?
    @ObservationIgnored private var replayRequestID: String?
    @ObservationIgnored private var replaySnapshotSequence: UInt64?
    @ObservationIgnored private var previewSelections: [String: FrontendPickerOption] = [:]
    @ObservationIgnored private var appIsInBackground = true

    init(
        client: GatewayClient? = nil,
        store: GatewayStore? = nil,
        settingsDefaults: UserDefaults = .standard,
        appLockAuthenticator: AppLockAuthenticator? = nil,
        requestSender: (@MainActor @Sendable (GatewayRequest) async throws -> Void)? = nil
    ) {
        let client = client ?? GatewayClient()
        let store = store ?? GatewayStore()
        let appLockAuthenticator = appLockAuthenticator ?? AppLockAuthenticator()
        let appLockEnabled = settingsDefaults.bool(forKey: appLockEnabledKey)
        self.client = client
        self.store = store
        self.settingsDefaults = settingsDefaults
        self.appLockAuthenticator = appLockAuthenticator
        self.requestSender = requestSender ?? { request in
            try await client.send(request)
        }
        self.accounts = store.loadAccounts()
        self.selectedAccountID = store.selectedAccountID()
        self.theme = ThemePreference(rawValue: UserDefaults.standard.string(forKey: "theme") ?? "") ?? .system
        self.appLockEnabled = appLockEnabled
        self.isAppLocked = appLockEnabled
        self.appLockAuthenticationMethod = appLockAuthenticator.method
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
        deltaFlushTask?.cancel()
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
            && attachmentUploadRequests.isEmpty
            && applyState != .applying
            && applyState != .restarting
    }

    var canCreateSession: Bool { canOpenSession }

    var isSwitchingGitBranch: Bool { gitBranchRequestID != nil }

    var attachmentsEnabled: Bool {
        contributions.contains { $0.acceptsFileAttachments }
    }

    var selectedRouteSupportsImageInput: Bool {
        modelChoices.first(where: { $0.route == selectedModelRoute })?
            .supportsImageInput == true
    }

    var canSubmitAttachments: Bool {
        attachmentsEnabled
            && (selectedRouteSupportsImageInput || !uploadedComposerAttachments.contains {
                $0.mediaType.hasPrefix("image/")
            })
    }

    var attachmentSubmissionUnavailableMessage: String {
        attachmentsEnabled
            ? "The selected model does not accept image attachments."
            : "File attachments are not enabled for this chat."
    }

    var canImportAttachments: Bool {
        attachmentsEnabled
            && connectionState.isReady
            && selectedSessionID != nil
            && activeTurnID == nil
    }

    var canSendComposer: Bool {
        let hasText = !composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let uploaded = uploadedComposerAttachments
        guard uploaded.isEmpty || canSubmitAttachments else { return false }
        return hasText || !uploaded.isEmpty
    }

    private var uploadedComposerAttachments: [AttachmentRecord] {
        composerAttachments.compactMap { item in
            guard case .uploaded(let attachment) = item.state else { return nil }
            return attachment
        }
    }

    var composerHasUnfinishedAttachments: Bool {
        composerAttachments.contains { item in
            switch item.state {
            case .uploaded: false
            case .queued, .uploading, .failed: true
            }
        }
    }

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

    var providerDraft: ProviderConfig? {
        get { agentDraft?.provider ?? setupProviderDraft }
        set {
            guard let newValue else {
                setupProviderDraft = nil
                return
            }
            if agentDraft != nil {
                agentDraft?.provider = newValue
            } else {
                setupProviderDraft = newValue
            }
        }
    }

    var contextFillFraction: Double {
        guard let modelContextWindow, modelContextWindow > 0 else { return 0 }
        return min(max(Double(contextTokens) / Double(modelContextWindow), 0), 1)
    }

    var contextFillPercent: Int {
        Int((contextFillFraction * 100).rounded())
    }

    /// Completed execution time plus the live turn, when one is running.
    func sessionElapsed(at date: Date) -> TimeInterval {
        let completed = TimeInterval(runStats.elapsedMs) / 1_000
        if let active = runStats.active {
            let live = max(
                TimeInterval(active.elapsedMs) / 1_000,
                date.timeIntervalSince1970 - TimeInterval(active.startedAtMs) / 1_000
            )
            return completed + max(0, live)
        }
        guard let session = sessions.first(where: { $0.sessionId == selectedSessionID }),
              session.activity.state != .idle
        else { return completed }
        guard let startedAt = session.activity.startedAt else { return completed }
        return completed + max(0, date.timeIntervalSince1970 - TimeInterval(startedAt))
    }

    var sessionRunCount: UInt64 { runStats.runCount + (runStats.active == nil ? 0 : 1) }
    var sessionModelCalls: UInt64 { runStats.modelCalls + (runStats.active?.modelCalls ?? 0) }
    var sessionToolCalls: UInt64 { runStats.toolCalls + (runStats.active?.toolCalls ?? 0) }
    var sessionFailedToolCalls: UInt64 {
        runStats.failedToolCalls + (runStats.active?.failedToolCalls ?? 0)
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

    var headerWidgets: [MountedWidget] { widgets(in: .header) }
    var composerHeaderWidgets: [MountedWidget] { widgets(in: .composerHeader) }
    var composerFooterWidgets: [MountedWidget] { widgets(in: .composerFooter) }
    var messageActionWidgets: [MountedWidget] {
        widgets(in: .messageActions).filter { $0.widget.action != nil }
    }
    var navigationWidgets: [MountedWidget] { widgets(in: .navigation) }
    var chatMenuWidgets: [MountedWidget] { widgets(in: .chatMenu) }

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

    func renameSelectedGateway(_ name: String) {
        guard let account = selectedAccount else { return }
        do {
            let renamed = try store.rename(account, to: name)
            guard let index = accounts.firstIndex(where: { $0.id == renamed.id }) else { return }
            accounts[index] = renamed
            showToast("Gateway renamed.", tone: .success)
        } catch {
            showToast(error.localizedDescription, tone: .error)
        }
    }

    func reconnect() {
        guard let account = selectedAccount else { return }
        connect(to: account)
    }

    func setSceneActive(_ active: Bool) {
        guard active else {
            reconnectsOnActivation = true
            return
        }
        guard reconnectsOnActivation, pendingPairingAccount == nil else { return }
        reconnectsOnActivation = false
        reconnect()
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
        let cached = selectedAccountID.flatMap {
            store.loadTranscript(accountID: $0, sessionID: sessionID)
        }
        requestSessionOpen(
            sessionID,
            lastSequence: cached?.sequence,
            replayEpoch: cached?.replayEpoch,
            cachedTranscript: cached,
            presentedTranscript: cached?.transcript
        )
    }

    func restoreSession(_ sessionID: String) {
        flushStreamDeltas()
        guard sessionID == selectedSessionID,
              let sequence = latestSequence,
              let epoch = currentReplayEpoch
        else {
            requestSessionOpen(sessionID, lastSequence: nil, replayEpoch: nil)
            return
        }
        let base = CachedTranscript(
            replayEpoch: epoch,
            sequence: sequence,
            transcript: transcript,
            currentUsage: currentUsage,
            lastUsage: lastUsage
        )
        let presentation = CachedTranscript(
            replayEpoch: epoch,
            sequence: sequence,
            transcript: displayedTranscript,
            currentUsage: currentUsage,
            lastUsage: lastUsage
        ).transcript
        requestSessionOpen(
            sessionID,
            lastSequence: sequence,
            replayEpoch: epoch,
            cachedTranscript: base,
            presentedTranscript: presentation
        )
    }

    private func requestSessionOpen(
        _ sessionID: String,
        lastSequence: UInt64?,
        replayEpoch: String?,
        cachedTranscript: CachedTranscript? = nil,
        presentedTranscript: [TranscriptEntry]? = nil
    ) {
        if sessionID != selectedSessionID {
            discardComposerAttachments()
            discardAttachmentPreview()
        }
        sessionToRestoreID = nil
        sessionOpeningID = sessionID
        sessionOpenCursor = lastSequence
        pendingCachedTranscript = cachedTranscript
        pendingPresentedTranscript = presentedTranscript
        let id = requestID("open")
        sessionRequestID = id
        connectionState = .loading
        transmit(.openSession(
            requestID: id,
            sessionID: sessionID,
            lastSequence: lastSequence,
            replayEpoch: replayEpoch
        )) { [weak self] _ in
            guard self?.sessionRequestID == id else { return }
            self?.sessionRequestID = nil
            self?.sessionOpeningID = nil
            self?.sessionOpenCursor = nil
            self?.pendingCachedTranscript = nil
            self?.pendingPresentedTranscript = nil
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
        if let accountID = selectedAccountID {
            store.removeTranscript(accountID: accountID, sessionID: session.sessionId)
        }
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
        guard let scope = workspaceViewerScope.gitScope else {
            refreshWorkspaceFiles()
            return
        }
        let id = requestID("git-diff")
        gitDiffRequestID = id
        isLoadingGitDiff = true
        transmit(.getGitDiff(requestID: id, sessionID: sessionID, scope: scope)) { [weak self] _ in
            guard self?.gitDiffRequestID == id else { return }
            self?.gitDiffRequestID = nil
            self?.isLoadingGitDiff = false
        }
    }

    func selectWorkspaceViewerScope(_ scope: WorkspaceViewerScope) {
        guard workspaceViewerScope != scope else { return }
        workspaceViewerScope = scope
        gitDiff = ""
        workspaceFiles = []
        refreshGitDiff()
    }

    private func refreshWorkspaceFiles() {
        guard connectionState.isReady, let sessionID = selectedSessionID else { return }
        let id = requestID("workspace-files")
        workspaceFilesRequestID = id
        isLoadingWorkspaceFiles = true
        transmit(.listWorkspaceFiles(requestID: id, sessionID: sessionID)) { [weak self] _ in
            guard self?.workspaceFilesRequestID == id else { return }
            self?.workspaceFilesRequestID = nil
            self?.isLoadingWorkspaceFiles = false
        }
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

    func importAttachments(_ urls: [URL]) async {
        guard canImportAttachments, let sessionID = selectedSessionID else { return }
        let generation = attachmentImportGeneration
        let available = max(
            0,
            maximumAttachmentReferences - composerAttachments.count - attachmentImportReservations
        )
        let selectedURLs = Array(urls.prefix(available))
        if urls.count > selectedURLs.count {
            showToast("You can attach up to 16 files to a message.", tone: .warning)
        }
        guard !selectedURLs.isEmpty else { return }

        var reservedCount = selectedURLs.count
        attachmentImportReservations += reservedCount
        defer { attachmentImportReservations -= reservedCount }
        for url in selectedURLs {
            guard generation == attachmentImportGeneration else { return }
            do {
                let imported = try await Self.loadImportedAttachment(url)
                attachmentImportReservations -= 1
                reservedCount -= 1
                guard generation == attachmentImportGeneration,
                      sessionID == selectedSessionID,
                      canImportAttachments
                else { return }
                let currentBytes = composerAttachments.reduce(Int64(0)) { total, attachment in
                    let (sum, overflow) = total.addingReportingOverflow(attachment.size)
                    return overflow || attachment.size < 0 ? .max : sum
                }
                if currentBytes > maximumComposerAttachmentBytes - Int64(imported.data.count) {
                    showToast(AttachmentImportError.totalTooLarge.localizedDescription, tone: .error)
                    continue
                }
                let id = UUID()
                attachmentData[id] = imported.data
                composerAttachments.append(ComposerAttachment(
                    id: id,
                    name: imported.name,
                    size: Int64(imported.data.count),
                    mediaType: imported.mediaType,
                    state: .queued
                ))
            } catch {
                attachmentImportReservations -= 1
                reservedCount -= 1
                guard generation == attachmentImportGeneration else { return }
                showToast(error.localizedDescription, tone: .error)
            }
        }
        startNextAttachmentUpload()
    }

    func removeComposerAttachment(_ id: UUID) {
        guard activeAttachmentUpload?.localID != id else { return }
        attachmentData[id] = nil
        composerAttachments.removeAll { $0.id == id }
    }

    func retryComposerAttachment(_ id: UUID) {
        guard attachmentData[id] != nil,
              let index = composerAttachments.firstIndex(where: { $0.id == id }),
              case .failed = composerAttachments[index].state
        else { return }
        composerAttachments[index].state = .queued
        startNextAttachmentUpload()
    }

    func refreshAttachments() {
        guard connectionState.isReady, let sessionID = selectedSessionID else { return }
        let id = requestID("attachments")
        attachmentListRequestID = id
        isLoadingAttachments = true
        transmit(.listAttachments(requestID: id, sessionID: sessionID)) { [weak self] _ in
            guard self?.attachmentListRequestID == id else { return }
            self?.attachmentListRequestID = nil
            self?.isLoadingAttachments = false
        }
    }

    func previewAttachment(_ attachment: AttachmentRecord) {
        guard let sessionID = selectedSessionID else { return }
        guard attachment.size <= Int64(maximumPreviewBytes) else {
            showToast("Quick Look previews are limited to 25 MiB.", tone: .warning)
            return
        }
        discardAttachmentPreview()
        let id = requestID("attachment-read")
        let generation = UUID()
        attachmentPreviewGeneration = generation
        attachmentPreviewDownload = AttachmentPreviewDownload(
            generation: generation,
            attachment: attachment,
            sessionID: sessionID,
            data: Data(),
            requestID: id
        )
        isLoadingAttachmentPreview = true
        transmit(.readAttachment(
            requestID: id,
            sessionID: sessionID,
            attachmentID: attachment.id,
            offset: 0,
            maxBytes: 256 * 1024
        )) { [weak self] message in
            guard self?.attachmentPreviewDownload?.requestID == id else { return }
            self?.attachmentPreviewDownload = nil
            self?.isLoadingAttachmentPreview = false
            self?.showToast(message, tone: .error)
        }
    }

    func previewWorkspaceFile(_ file: WorkspaceFileRecord) {
        guard let sessionID = selectedSessionID else { return }
        guard file.size <= UInt64(maximumPreviewBytes) else {
            showToast("Quick Look previews are limited to 25 MiB.", tone: .warning)
            return
        }
        discardAttachmentPreview()
        let id = requestID("workspace-file-read")
        let generation = UUID()
        attachmentPreviewGeneration = generation
        workspaceFilePreviewDownload = WorkspaceFilePreviewDownload(
            generation: generation,
            file: file,
            sessionID: sessionID,
            data: Data(),
            requestID: id
        )
        isLoadingAttachmentPreview = true
        transmit(.readWorkspaceFile(
            requestID: id,
            sessionID: sessionID,
            path: file.path,
            offset: 0,
            maxBytes: 256 * 1024
        )) { [weak self] message in
            guard self?.workspaceFilePreviewDownload?.requestID == id else { return }
            self?.workspaceFilePreviewDownload = nil
            self?.isLoadingAttachmentPreview = false
            self?.showToast(message, tone: .error)
        }
    }

    func discardAttachmentPreview() {
        attachmentPreviewGeneration = UUID()
        attachmentPreviewDownload = nil
        workspaceFilePreviewDownload = nil
        isLoadingAttachmentPreview = false
        if let previewTemporaryDirectory {
            try? FileManager.default.removeItem(at: previewTemporaryDirectory)
        }
        previewTemporaryDirectory = nil
        previewURL = nil
    }

    func sendMessage() {
        guard let sessionID = selectedSessionID else { return }
        let text = composer.trimmingCharacters(in: .whitespacesAndNewlines)
        let attachments = uploadedComposerAttachments
        guard attachments.count <= maximumAttachmentReferences else { return }
        guard !text.isEmpty || !attachments.isEmpty else { return }
        guard attachments.isEmpty || canSubmitAttachments else {
            showToast(attachmentSubmissionUnavailableMessage, tone: .warning)
            return
        }
        guard !composerHasUnfinishedAttachments else {
            showToast("Wait for attachments to finish uploading.", tone: .warning)
            return
        }
        guard text.utf8.count <= maximumComposerBytes else {
            showToast("Messages are limited to 1 MiB.", tone: .error)
            return
        }
        if activeTurnID != nil, !attachments.isEmpty {
            showToast("Attachments can be sent with a new turn.", tone: .warning)
            return
        }
        let id = requestID("input")
        let op: AgentOperation
        if let activeTurnID, let activeOperation {
            op = .activeInput(operation: activeOperation, turnID: activeTurnID, text: text)
        } else {
            op = .userInput(text: text, attachments: attachments)
        }
        pendingDrafts[id] = PendingComposerDraft(text: text, attachments: attachments)
        composer = ""
        composerAttachments = []
        transmit(.submit(sessionID: sessionID, submission: Submission(id: id, op: op))) { [weak self] _ in
            self?.restoreDraft(id: id)
        }
    }

    func refreshProfile() {
        guard connectionState.isReady else { return }
        transmit(.getProfile(requestID: requestID("profile")))
    }

    func submitWidget(_ mounted: MountedWidget) {
        guard let sessionID = selectedSessionID, let action = mounted.widget.action else { return }
        let id = requestID("widget")
        transmit(.submit(sessionID: sessionID, submission: Submission(id: id, op: action)))
    }

    func submitMessageAction(_ mounted: MountedWidget, target: MessageTarget) {
        guard let sessionID = selectedSessionID, let action = mounted.widget.action else { return }
        let submittedAction = switch action {
        case .capabilityCommand(let capability, let command, let arguments, let input, _):
            AgentOperation.capabilityCommand(
                capability: capability,
                command: command,
                arguments: arguments,
                input: input,
                target: target
            )
        default:
            action
        }
        transmit(.submit(
            sessionID: sessionID,
            submission: Submission(id: requestID("widget"), op: submittedAction)
        ))
    }

    func submitFrontendOperation(_ operation: AgentOperation) {
        guard let sessionID = selectedSessionID else { return }
        transmit(.submit(
            sessionID: sessionID,
            submission: Submission(id: requestID("widget-action"), op: operation)
        ))
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

    var agentDraftModelRoute: String? {
        guard let provider = agentDraft?.provider else { return nil }
        return modelChoices.first { choice in
            choice.model == provider.model
                && choice.reasoningEffort == provider.reasoningEffort
                && providerStatus(for: choice)?.provider == provider.provider
        }?.route
    }

    func selectAgentDraftModel(_ route: String) {
        guard let choice = modelChoices.first(where: { $0.route == route }),
              let status = providerStatus(for: choice),
              var provider = status.selection,
              var draft = agentDraft
        else { return }
        provider.model = choice.model
        provider.reasoningEffort = choice.reasoningEffort
        draft.provider = provider
        agentDraft = draft
    }

    func providerSymbol(for choice: ModelChoice) -> String? {
        providerStatus(for: choice)?.symbol
    }

    private func providerStatus(for choice: ModelChoice) -> ProviderStatus? {
        guard let provider = modelProviders[choice.route] else { return nil }
        return providerStatuses.first { $0.provider == provider }
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
        inspectorPage = .changes
        showsInspector = true
        refreshGitDiff()
    }

    func showInspector() {
        inspectorPage = .changes
        showsInspector = true
        refreshGitDiff()
    }

    func showUploadedFiles() {
        inspectorPage = .uploads
        showsInspector = true
        refreshAttachments()
    }

    func toggleInspector() {
        if showsInspector {
            showsInspector = false
        } else {
            showInspector()
        }
    }

    func changeAgentForCurrentChat() {
        applyAgentConfiguration(to: .session)
    }

    func saveAgentAsDefault() {
        applyAgentConfiguration(to: .defaultAgent)
    }

    func setApprovalPolicyForCurrentChat(_ policy: String) {
        guard let snapshot = agentSnapshot, let draft = agentDraft else { return }
        guard draft == snapshot.config else {
            showToast(
                "Apply or reload pending agent/provider edits before changing approval.",
                tone: .warning
            )
            return
        }
        guard draft.middleware.settings["sandbox"]?["approval_policy"] != .string(policy) else {
            return
        }
        agentDraft?.middleware.setSetting(
            .string(policy),
            middleware: "sandbox",
            setting: "approval_policy"
        )
        changeAgentForCurrentChat()
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
        providerDraft = status.selection ?? ProviderConfig(
            provider: status.provider,
            model: selectedModel?.id ?? status.modelIds.first ?? "",
            baseUrl: status.defaultBaseUrl,
            reasoningEffort: selectedModel?.defaultReasoning,
            webSearch: webSearch
        )
        providerModelIDsText = status.modelIds.joined(separator: ", ")
        providerAPIKey = ""
        providerActionState = .idle
    }

    var providerModelIDs: [String] {
        providerModelIDsText
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .reduce(into: []) { values, value in
                if !values.contains(value) { values.append(value) }
            }
    }

    func updateProviderModelIDs(_ value: String) {
        providerModelIDsText = value
        guard let first = providerModelIDs.first else { return }
        providerDraft?.model = first
        providerDraft?.reasoningEffort = nil
    }

    func selectProviderModel(_ modelID: String) {
        guard let status = providerStatuses.first(where: {
            $0.provider == providerDraft?.provider
        }) else { return }
        providerDraft?.model = modelID
        providerDraft?.reasoningEffort = status.models
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
        if let baseURL = providerDraft?.baseUrl {
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
        if case .session = target, selectedSessionID == nil { return }
        guard var config = providerDraft,
              let status = providerStatuses.first(where: { $0.provider == config.provider })
        else { return }
        let modelIDs = status.modelIdsConfigurable ? providerModelIDs : status.modelIds
        if status.modelIdsConfigurable {
            guard let first = modelIDs.first else { return }
            config.model = first
            config.reasoningEffort = nil
        }
        let id = requestID("provider")
        providerRegistrationRequestID = id
        providerRegistrationTarget = target
        applyState = .applying
        transmit(.registerProvider(requestID: id, config: config, modelIds: modelIDs)) { [weak self] message in
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

    func refreshAppLockAuthenticationMethod() {
        appLockAuthenticationMethod = appLockAuthenticator.method
    }

    func setAppLockEnabled(_ enabled: Bool) async {
        guard enabled != appLockEnabled, !isAppLockAuthenticating else { return }
        guard enabled else {
            appLockEnabled = false
            isAppLocked = false
            appLockError = nil
            settingsDefaults.set(false, forKey: appLockEnabledKey)
            return
        }
        guard await authenticateForAppLock(
            reason: "Authenticate to enable app lock in Horus."
        ) else { return }
        appLockEnabled = true
        isAppLocked = appIsInBackground
        settingsDefaults.set(true, forKey: appLockEnabledKey)
    }

    func appDidEnterBackground() {
        appIsInBackground = true
        guard appLockEnabled else { return }
        isAppLocked = true
        appLockError = nil
    }

    func appDidBecomeActive() async {
        appIsInBackground = false
        await unlockApp()
    }

    func unlockApp() async {
        guard appLockEnabled, isAppLocked, !isAppLockAuthenticating else { return }
        guard await authenticateForAppLock(reason: "Authenticate to unlock Horus.") else {
            return
        }
        isAppLocked = appIsInBackground
    }

    private func authenticateForAppLock(reason: String) async -> Bool {
        refreshAppLockAuthenticationMethod()
        guard appLockAuthenticationMethod.isAvailable else {
            appLockError = "Biometric authentication is unavailable. Update Face ID or Touch ID, then try again."
            return false
        }
        isAppLockAuthenticating = true
        appLockError = nil
        let succeeded = await appLockAuthenticator.authenticate(reason: reason)
        isAppLockAuthenticating = false
        guard succeeded else {
            appLockError = "Authentication wasn’t completed. Try again."
            return false
        }
        return true
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

    func handle(_ envelope: GatewayEnvelope) {
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
            applySessionReady(payload, opened: true, replayRequestID: requestID)
        case .sessionReplayComplete(let requestID, let sessionID):
            guard requestID == replayRequestID, sessionID == selectedSessionID else { break }
            finishSessionReplay()
        case .sessionHistory:
            break
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
            let buffered = BufferedAgentEvent(
                sequence: sequence,
                event: event,
                blocks: blocks,
                history: history,
                preview: preview
            )
            applyAgentEvent(buffered)
            if replayRequestID == nil, shouldCacheTranscript(after: event) {
                cacheSelectedTranscript()
            }
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
        case .gitDiff(let requestID, let sessionID, let scope, let diff):
            guard requestID == gitDiffRequestID,
                  sessionID == selectedSessionID,
                  scope == workspaceViewerScope.gitScope
            else { break }
            gitDiffRequestID = nil
            isLoadingGitDiff = false
            gitDiff = diff
        case .workspaceFiles(let requestID, let sessionID, let files):
            guard requestID == workspaceFilesRequestID,
                  sessionID == selectedSessionID,
                  workspaceViewerScope == .all
            else { break }
            workspaceFilesRequestID = nil
            isLoadingWorkspaceFiles = false
            workspaceFiles = files
        case .workspaceFileChunk(
            let requestID,
            let sessionID,
            let path,
            let offset,
            let data,
            let nextOffset
        ):
            handleWorkspaceFileChunk(
                requestID: requestID,
                sessionID: sessionID,
                path: path,
                offset: offset,
                data: data,
                nextOffset: nextOffset
            )
        case .attachmentUploadStarted(let requestID, let sessionID, let uploadID, let maxChunkBytes):
            handleAttachmentUploadStarted(
                requestID: requestID,
                sessionID: sessionID,
                uploadID: uploadID,
                maxChunkBytes: maxChunkBytes
            )
        case .attachmentChunkAccepted(let requestID, let sessionID, let uploadID, let nextOffset):
            handleAttachmentChunkAccepted(
                requestID: requestID,
                sessionID: sessionID,
                uploadID: uploadID,
                nextOffset: nextOffset
            )
        case .attachmentUploaded(let requestID, let sessionID, let attachment):
            handleAttachmentUploaded(
                requestID: requestID,
                sessionID: sessionID,
                attachment: attachment
            )
        case .attachments(let requestID, let sessionID, let attachments):
            guard requestID == attachmentListRequestID, sessionID == selectedSessionID else { break }
            attachmentListRequestID = nil
            isLoadingAttachments = false
            uploadedAttachments = attachments
        case .attachmentChunk(
            let requestID,
            let sessionID,
            let attachmentID,
            let offset,
            let data,
            let nextOffset
        ):
            handleAttachmentChunk(
                requestID: requestID,
                sessionID: sessionID,
                attachmentID: attachmentID,
                offset: offset,
                data: data,
                nextOffset: nextOffset
            )
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

    private func applyAgentEvent(_ buffered: BufferedAgentEvent) {
        guard latestSequence.map({ buffered.sequence > $0 }) ?? true else { return }
        latestSequence = buffered.sequence
        reduce(
            event: buffered.event,
            blocks: buffered.blocks,
            history: buffered.history,
            preview: buffered.preview
        )
    }

    private func finishSessionReplay() {
        flushStreamDeltas()
        if let replaySnapshotSequence { latestSequence = replaySnapshotSequence }
        replayRequestID = nil
        replaySnapshotSequence = nil
        replayPresentedTranscript = nil
        connectionState = .ready
        cacheSelectedTranscript()
    }

    private func shouldCacheTranscript(after event: AgentEventRecord) -> Bool {
        switch event.msg["type"]?.stringValue {
        case "session_history", "task_complete", "turn_aborted": true
        default: false
        }
    }

    private func cacheSelectedTranscript() {
        guard let accountID = selectedAccountID,
              let sessionID = selectedSessionID,
              let currentReplayEpoch,
              let latestSequence,
              activeTurnID == nil,
              pendingApproval == nil
        else { return }
        store.saveTranscript(
            accountID: accountID,
            sessionID: sessionID,
            replayEpoch: currentReplayEpoch,
            sequence: latestSequence,
            transcript: transcript,
            currentUsage: currentUsage,
            lastUsage: lastUsage
        )
    }

    private func applyGatewayReady(_ payload: ReadyPayload) {
        applyGatewayCatalog(payload)
        if sessionRequestID == nil { connectionState = .ready }
        applySessions(payload.sessions)
        refreshProfile()
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
            applyState = .idle
            if selectedSessionID != nil,
               let snapshot = agentSnapshot,
               let draft = agentDraft,
               draft == payload.defaultConfig?.config,
               draft != snapshot.config {
                applyAgentConfiguration(to: .session)
            } else {
                applyState = .applied
                showToast("Default agent saved for new chats.", tone: .success)
            }
        }
    }

    func applyGatewayCatalog(_ payload: ReadyPayload) {
        let previousDefault = defaultAgentSnapshot
        providerStatuses = payload.providers
        modelChoices = payload.models
        modelProviders = payload.modelProviders
        middlewareFeatures = payload.middlewareFeatures
        defaultAgentSnapshot = payload.defaultConfig
        if agentSnapshot == nil {
            agentDraft = payload.defaultConfig.map {
                refreshedAgentDraft(
                    currentDraft: agentDraft,
                    currentSnapshot: previousDefault,
                    incomingSnapshot: $0
                )
            }
        }
        if providerDraft == nil, let provider = providerStatuses.first {
            selectProvider(provider.provider)
        }
    }

    private func applySessionReady(
        _ payload: SessionReadyPayload,
        opened: Bool,
        replayRequestID: String? = nil
    ) {
        let cursor = sessionOpenCursor
        let cached = opened && sessionOpeningID == payload.session.sessionId
            ? pendingCachedTranscript
            : nil
        let presented = opened && sessionOpeningID == payload.session.sessionId
            ? pendingPresentedTranscript
            : nil
        if selectedSessionID != payload.session.sessionId {
            restorePendingDrafts()
            resetSessionState()
        }
        if opened {
            latestSequence = cursor
            currentReplayEpoch = payload.replayEpoch
            self.replayRequestID = replayRequestID
            replaySnapshotSequence = payload.latestSequence
            sessionOpenCursor = nil
            sessionOpeningID = nil
            pendingCachedTranscript = nil
            pendingPresentedTranscript = nil
            replayPresentedTranscript = presented ?? []
            transcript = cached?.transcript ?? []
            if let cached {
                currentUsage = cached.currentUsage
                lastUsage = cached.lastUsage
                updateContextTokens()
            }
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
            contribution.widgets.map {
                MountedWidget(capability: contribution.capability, widget: $0)
            }
        }
        for widget in payload.widgets {
            upsertWidget(MountedWidget(capability: widget.capability, widget: widget.item))
        }
        runStats = payload.runStats
        activeOperation = payload.contributions.compactMap(\.activeInput?.operation).first
        agentDraft = refreshedAgentDraft(
            currentDraft: agentDraft,
            currentSnapshot: agentSnapshot,
            incomingSnapshot: payload.config
        )
        agentSnapshot = payload.config
        if !opened { connectionState = .ready }
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
        if let selected = sessions.first(where: { $0.sessionId == selectedSessionID }) {
            applyExecutionStats(selected.executionStats)
            if selected.activity.state == .idle { runStats.active = nil }
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

    private func applyExecutionStats(_ stats: ExecutionStats) {
        runStats.runCount = stats.runCount
        runStats.failedRunCount = stats.failedRunCount
        runStats.abortedRunCount = stats.abortedRunCount
        runStats.modelCalls = stats.modelCalls
        runStats.toolCalls = stats.toolCalls
        runStats.failedToolCalls = stats.failedToolCalls
        runStats.elapsedMs = stats.elapsedMs
        runStats.usage = stats.usage
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
        refreshAttachments()
        refreshCron()
    }

    private func refreshArtifacts() {
        guard let sessionID = selectedSessionID else { return }
        transmit(.listArtifacts(requestID: requestID("artifacts"), sessionID: sessionID))
    }

    private func clearSelectedSession() {
        latestSequence = nil
        currentReplayEpoch = nil
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
        if rejection.requestId == sessionRequestID,
           rejection.code == "replay_unavailable",
           let sessionID = sessionOpeningID,
           sessionOpenCursor != nil {
            if let accountID = selectedAccountID {
                store.removeTranscript(accountID: accountID, sessionID: sessionID)
            }
            sessionRequestID = nil
            sessionOpenCursor = nil
            pendingCachedTranscript = nil
            pendingPresentedTranscript = nil
            if sessionID == selectedSessionID { resetSessionState() }
            requestSessionOpen(sessionID, lastSequence: nil, replayEpoch: nil)
            return
        }
        failAttachmentRequest(rejection.requestId, message: rejection.message, showsToast: false)
        if rejection.requestId == attachmentListRequestID {
            attachmentListRequestID = nil
            isLoadingAttachments = false
        }
        if rejection.requestId == attachmentPreviewDownload?.requestID {
            attachmentPreviewDownload = nil
            isLoadingAttachmentPreview = false
        }
        if rejection.requestId == workspaceFilePreviewDownload?.requestID {
            workspaceFilePreviewDownload = nil
            isLoadingAttachmentPreview = false
        }
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
            sessionOpeningID = nil
            sessionOpenCursor = nil
            pendingCachedTranscript = nil
            pendingPresentedTranscript = nil
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
            isLoadingGitDiff = false
        }
        if rejection.requestId == workspaceFilesRequestID {
            workspaceFilesRequestID = nil
            isLoadingWorkspaceFiles = false
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
        // Anything that is not a delta may read or finalize the streams the buffer feeds,
        // so buffered text must land first to keep transcript order exact.
        if type != "agent_message_content_delta", type != "agent_reasoning_content_delta" {
            flushStreamDeltas()
        }
        if type == "session_history" {
            transcript = []
            currentUsage = TokenUsage()
            lastUsage = TokenUsage()
            contextTokens = 0
            for rendered in history ?? [] {
                guard rendered.event["frontendType"]?.stringValue != "picker" else { continue }
                reduce(
                    event: AgentEventRecord(submissionId: nil, msg: rendered.event),
                    blocks: rendered.blocks,
                    preview: nil
                )
            }
            return
        }
        let wasRendered = !blocks.isEmpty
        if let submissionID = event.submissionId {
            if type == "warning" || type == "error" {
                if let draft = pendingDrafts.removeValue(forKey: submissionID) { restoreDraft(draft) }
                previewSelections.removeValue(forKey: submissionID)
            } else {
                pendingDrafts.removeValue(forKey: submissionID)
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
        case "user_message":
            let attachments = event.msg["attachments"]?.arrayValue?.compactMap {
                try? AttachmentRecord(json: $0)
            } ?? []
            appendText(
                event.msg["message"]?.stringValue,
                kind: .user,
                messageTarget: messageTarget(from: event.msg),
                attachments: attachments
            )
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
                completeStream(
                    text: event.msg["message"]?.stringValue ?? "",
                    kind: kind,
                    messageTarget: messageTarget(from: event.msg)
                )
            }
        case "task_started":
            activeTurnID = event.msg["turnId"]?.stringValue
            if replayRequestID == nil,
               let turnID = activeTurnID,
               runStats.active?.turnId != turnID {
                runStats.active = RunSummary(
                    sessionId: selectedSessionID ?? "",
                    submissionId: event.submissionId ?? "",
                    turnId: turnID,
                    startedAtMs: Int64(Date.now.timeIntervalSince1970 * 1_000),
                    finishedAtMs: nil,
                    elapsedMs: 0,
                    outcome: nil,
                    modelCalls: 0,
                    toolCalls: 0,
                    failedToolCalls: 0,
                    usage: TokenUsage()
                )
            }
            if let window = event.msg["modelContextWindow"]?.intValue {
                modelContextWindow = Int64(window)
            }
        case "task_complete":
            finishPendingTranscriptEntries()
            activeTurnID = nil
            if replayRequestID == nil { runStats.active = nil }
            refreshGitDiff()
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
            if replayRequestID == nil { runStats.active = nil }
            refreshGitDiff()
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
        case "tool_call_begin":
            if replayRequestID == nil { runStats.active?.toolCalls += 1 }
        case "tool_call_end":
            if replayRequestID == nil, event.msg["isError"]?.boolValue == true {
                runStats.active?.failedToolCalls += 1
            }
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
                updateContextTokens()
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
            upsertWidget(MountedWidget(capability: capability, widget: widget))
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

    private func upsertWidget(_ mounted: MountedWidget) {
        if let index = mountedWidgets.firstIndex(where: { $0.id == mounted.id }) {
            mountedWidgets[index] = mounted
        } else {
            mountedWidgets.append(mounted)
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
        tone: String = "neutral",
        messageTarget: MessageTarget? = nil,
        attachments: [AttachmentRecord] = []
    ) {
        let text = text ?? ""
        guard !text.isEmpty || !attachments.isEmpty else { return }
        transcript.append(TranscriptEntry(
            id: UUID().uuidString,
            text: text,
            kind: kind,
            format: "plain_text",
            tone: tone,
            pending: false,
            messageTarget: messageTarget,
            attachments: attachments
        ))
    }

    // Deltas arrive several times per frame, and every application re-lays-out the whole
    // growing message. Batching to ~20 flushes a second keeps the text pipeline off the
    // critical path; ordering against non-delta events is preserved by the flush in `reduce`.
    private func appendStream(id: String, delta: String, kind: TranscriptEntry.Kind) {
        guard !delta.isEmpty else { return }
        if let last = bufferedDeltas.indices.last, bufferedDeltas[last].id == id {
            bufferedDeltas[last].delta += delta
        } else {
            bufferedDeltas.append((id: id, delta: delta, kind: kind))
        }
        guard deltaFlushTask == nil else { return }
        deltaFlushTask = Task { [weak self] in
            do {
                try await Task.sleep(for: .milliseconds(50))
            } catch {
                return
            }
            self?.flushStreamDeltas()
        }
    }

    private func flushStreamDeltas() {
        deltaFlushTask?.cancel()
        deltaFlushTask = nil
        for buffered in bufferedDeltas {
            if let index = transcript.lastIndex(where: { $0.id == buffered.id }) {
                transcript[index].text.append(buffered.delta)
            } else {
                transcript.append(TranscriptEntry(
                    id: buffered.id,
                    text: buffered.delta,
                    kind: buffered.kind,
                    format: "plain_text",
                    tone: "neutral",
                    pending: true
                ))
            }
        }
        bufferedDeltas.removeAll()
    }

    private func completeStream(
        text: String,
        kind: TranscriptEntry.Kind,
        messageTarget: MessageTarget?
    ) {
        if let index = transcript.lastIndex(where: { $0.pending && $0.kind == kind }) {
            transcript[index].text = text
            transcript[index].pending = false
            transcript[index].messageTarget = messageTarget
        } else {
            appendText(text, kind: kind, messageTarget: messageTarget)
        }
    }

    private func messageTarget(from event: JSONValue) -> MessageTarget? {
        event["messageTarget"].flatMap { MessageTarget(json: $0) }
    }

    private func finishPendingTranscriptEntries() {
        for entry in transcript where entry.pending {
            entry.pending = false
        }
    }

    private func updateContextTokens() {
        contextTokens = max(
            0,
            max(lastUsage.totalTokens, lastUsage.inputTokens + lastUsage.outputTokens)
        )
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

    private nonisolated static func loadImportedAttachment(
        _ url: URL
    ) async throws -> ImportedAttachmentData {
        try await Task.detached(priority: .userInitiated) {
            let accessed = url.startAccessingSecurityScopedResource()
            defer { if accessed { url.stopAccessingSecurityScopedResource() } }

            let values = try url.resourceValues(forKeys: [
                .isRegularFileKey,
                .fileSizeKey,
                .contentTypeKey,
            ])
            guard values.isRegularFile == true else { throw AttachmentImportError.notAFile }
            if let size = values.fileSize, size > maximumAttachmentBytes {
                throw AttachmentImportError.tooLarge
            }
            let data = try Data(contentsOf: url)
            guard data.count <= maximumAttachmentBytes else { throw AttachmentImportError.tooLarge }
            if let size = values.fileSize, size != data.count {
                throw AttachmentImportError.changedWhileReading
            }
            let mediaType = values.contentType?.preferredMIMEType
                ?? UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
                ?? "application/octet-stream"
            return ImportedAttachmentData(
                name: url.lastPathComponent,
                mediaType: mediaType,
                data: data
            )
        }.value
    }

    private func startNextAttachmentUpload() {
        guard connectionState.isReady,
              activeAttachmentUpload == nil,
              attachmentUploadRequests.isEmpty,
              let sessionID = selectedSessionID,
              let index = composerAttachments.firstIndex(where: {
                  if case .queued = $0.state { return true }
                  return false
              }),
              attachmentData[composerAttachments[index].id] != nil
        else { return }

        let item = composerAttachments[index]
        composerAttachments[index].state = .uploading
        let id = requestID("attachment-begin")
        attachmentUploadRequests[id] = .begin(localID: item.id)
        transmit(.beginAttachmentUpload(
            requestID: id,
            sessionID: sessionID,
            name: item.name,
            size: item.size,
            mediaType: item.mediaType
        )) { [weak self] message in
            self?.failAttachmentRequest(id, message: message, showsToast: false)
        }
    }

    private func handleAttachmentUploadStarted(
        requestID: String,
        sessionID: String,
        uploadID: String,
        maxChunkBytes: Int
    ) {
        guard let request = attachmentUploadRequests[requestID] else { return }
        guard case .begin(let localID) = request else {
            return failAttachment(request.localID, message: "The gateway returned an invalid upload.")
        }
        guard sessionID == selectedSessionID,
              !uploadID.isEmpty,
              maxChunkBytes > 0,
              maxChunkBytes <= maximumGatewayFrameBytes
        else { return failAttachment(localID, message: "The gateway returned an invalid upload.") }
        attachmentUploadRequests.removeValue(forKey: requestID)
        activeAttachmentUpload = ActiveAttachmentUpload(
            localID: localID,
            sessionID: sessionID,
            uploadID: uploadID,
            maxChunkBytes: min(maxChunkBytes, 256 * 1024)
        )
        sendNextAttachmentChunk(localID: localID, offset: 0)
    }

    private func handleAttachmentChunkAccepted(
        requestID: String,
        sessionID: String,
        uploadID: String,
        nextOffset: Int64
    ) {
        guard let request = attachmentUploadRequests[requestID] else { return }
        guard case .append(let localID, let expectedNextOffset) = request else {
            return failAttachment(request.localID, message: "The gateway returned an invalid upload.")
        }
        guard let upload = activeAttachmentUpload,
              upload.localID == localID,
              upload.sessionID == sessionID,
              upload.uploadID == uploadID
        else {
            return failAttachment(localID, message: "The gateway returned an invalid upload.")
        }
        guard nextOffset == expectedNextOffset else {
            return failAttachment(localID, message: "The gateway returned an invalid upload offset.")
        }
        attachmentUploadRequests.removeValue(forKey: requestID)
        sendNextAttachmentChunk(localID: localID, offset: nextOffset)
    }

    private func sendNextAttachmentChunk(localID: UUID, offset: Int64) {
        guard let upload = activeAttachmentUpload,
              upload.localID == localID,
              let data = attachmentData[localID],
              offset >= 0,
              let start = Int(exactly: offset),
              start <= data.count
        else {
            failAttachment(localID, message: "The gateway returned an invalid upload offset.")
            return
        }
        guard start < data.count else {
            let id = requestID("attachment-finish")
            attachmentUploadRequests[id] = .finish(localID: localID)
            transmit(.finishAttachmentUpload(
                requestID: id,
                sessionID: upload.sessionID,
                uploadID: upload.uploadID
            )) { [weak self] message in
                self?.failAttachmentRequest(id, message: message, showsToast: false)
            }
            return
        }

        let end = min(start + upload.maxChunkBytes, data.count)
        let id = requestID("attachment-chunk")
        attachmentUploadRequests[id] = .append(
            localID: localID,
            expectedNextOffset: Int64(end)
        )
        transmit(.appendAttachmentChunk(
            requestID: id,
            sessionID: upload.sessionID,
            uploadID: upload.uploadID,
            offset: offset,
            data: Data(data[start..<end])
        )) { [weak self] message in
            self?.failAttachmentRequest(id, message: message, showsToast: false)
        }
    }

    private func handleAttachmentUploaded(
        requestID: String,
        sessionID: String,
        attachment: AttachmentRecord
    ) {
        guard let request = attachmentUploadRequests[requestID] else { return }
        guard case .finish(let localID) = request else {
            return failAttachment(request.localID, message: "The gateway returned an invalid attachment.")
        }
        guard sessionID == selectedSessionID,
              activeAttachmentUpload?.localID == localID,
              activeAttachmentUpload?.sessionID == sessionID,
              let index = composerAttachments.firstIndex(where: { $0.id == localID }),
              composerAttachments[index].name == attachment.name,
              composerAttachments[index].size == attachment.size,
              composerAttachments[index].mediaType == attachment.mediaType
        else {
            return failAttachment(localID, message: "The gateway returned an invalid attachment.")
        }
        attachmentUploadRequests.removeValue(forKey: requestID)
        composerAttachments[index].state = .uploaded(attachment)
        attachmentData[localID] = nil
        activeAttachmentUpload = nil
        upsertUploadedAttachment(attachment)
        startNextAttachmentUpload()
    }

    @discardableResult
    private func failAttachmentRequest(
        _ requestID: String,
        message: String,
        showsToast: Bool = true
    ) -> Bool {
        guard let request = attachmentUploadRequests.removeValue(forKey: requestID) else {
            return false
        }
        failAttachment(request.localID, message: message, showsToast: showsToast)
        return true
    }

    private func failAttachment(
        _ localID: UUID,
        message: String,
        showsToast: Bool = true
    ) {
        attachmentUploadRequests = attachmentUploadRequests.filter { _, request in
            request.localID != localID
        }
        if activeAttachmentUpload?.localID == localID { activeAttachmentUpload = nil }
        if let index = composerAttachments.firstIndex(where: { $0.id == localID }) {
            composerAttachments[index].state = .failed(message)
        }
        if showsToast { showToast(message, tone: .error) }
        startNextAttachmentUpload()
    }

    private func upsertUploadedAttachment(_ attachment: AttachmentRecord) {
        if let index = uploadedAttachments.firstIndex(where: { $0.id == attachment.id }) {
            uploadedAttachments[index] = attachment
        } else {
            uploadedAttachments.append(attachment)
        }
    }

    private func discardComposerAttachments() {
        attachmentImportGeneration = UUID()
        composerAttachments.removeAll()
        attachmentData.removeAll()
    }

    private func discardPendingComposerAttachments() {
        attachmentImportGeneration = UUID()
        composerAttachments.removeAll { item in
            if case .uploaded = item.state { return false }
            return true
        }
        attachmentData.removeAll()
    }

    private func handleAttachmentChunk(
        requestID: String,
        sessionID: String,
        attachmentID: String,
        offset: Int64,
        data: Data,
        nextOffset: Int64?
    ) {
        guard var download = attachmentPreviewDownload else { return }
        guard download.requestID == requestID,
              download.sessionID == sessionID,
              download.attachment.id == attachmentID,
              offset == Int64(download.data.count),
              data.count <= 256 * 1024,
              Int64(download.data.count + data.count) <= download.attachment.size
        else {
            attachmentPreviewDownload = nil
            isLoadingAttachmentPreview = false
            showToast("The gateway returned an invalid attachment.", tone: .error)
            return
        }
        download.data.append(data)
        if let nextOffset {
            guard nextOffset == Int64(download.data.count), nextOffset > offset else {
                attachmentPreviewDownload = nil
                isLoadingAttachmentPreview = false
                showToast("The gateway returned an invalid attachment offset.", tone: .error)
                return
            }
            let id = self.requestID("attachment-read")
            download.requestID = id
            attachmentPreviewDownload = download
            transmit(.readAttachment(
                requestID: id,
                sessionID: sessionID,
                attachmentID: attachmentID,
                offset: nextOffset,
                maxBytes: 256 * 1024
            )) { [weak self] message in
                guard self?.attachmentPreviewDownload?.requestID == id else { return }
                self?.attachmentPreviewDownload = nil
                self?.isLoadingAttachmentPreview = false
                self?.showToast(message, tone: .error)
            }
            return
        }

        guard Int64(download.data.count) == download.attachment.size else {
            attachmentPreviewDownload = nil
            isLoadingAttachmentPreview = false
            showToast("The downloaded attachment is incomplete.", tone: .error)
            return
        }
        attachmentPreviewDownload = nil
        finishFilePreview(
            download.data,
            name: download.attachment.name,
            generation: download.generation
        )
    }

    private func handleWorkspaceFileChunk(
        requestID: String,
        sessionID: String,
        path: String,
        offset: UInt64,
        data: Data,
        nextOffset: UInt64?
    ) {
        guard var download = workspaceFilePreviewDownload else { return }
        guard download.requestID == requestID,
              download.sessionID == sessionID,
              download.file.path == path,
              offset == UInt64(download.data.count),
              data.count <= 256 * 1024,
              offset <= download.file.size,
              UInt64(data.count) <= download.file.size - offset
        else {
            workspaceFilePreviewDownload = nil
            isLoadingAttachmentPreview = false
            showToast("The gateway returned an invalid workspace file.", tone: .error)
            return
        }
        download.data.append(data)
        if let nextOffset {
            guard nextOffset == UInt64(download.data.count), nextOffset > offset else {
                workspaceFilePreviewDownload = nil
                isLoadingAttachmentPreview = false
                showToast("The gateway returned an invalid workspace file offset.", tone: .error)
                return
            }
            let id = self.requestID("workspace-file-read")
            download.requestID = id
            workspaceFilePreviewDownload = download
            transmit(.readWorkspaceFile(
                requestID: id,
                sessionID: sessionID,
                path: path,
                offset: nextOffset,
                maxBytes: 256 * 1024
            )) { [weak self] message in
                guard self?.workspaceFilePreviewDownload?.requestID == id else { return }
                self?.workspaceFilePreviewDownload = nil
                self?.isLoadingAttachmentPreview = false
                self?.showToast(message, tone: .error)
            }
            return
        }

        guard UInt64(download.data.count) == download.file.size else {
            workspaceFilePreviewDownload = nil
            isLoadingAttachmentPreview = false
            showToast("The downloaded workspace file is incomplete.", tone: .error)
            return
        }
        workspaceFilePreviewDownload = nil
        finishFilePreview(
            download.data,
            name: URL(fileURLWithPath: download.file.path).lastPathComponent,
            generation: download.generation
        )
    }

    private func finishFilePreview(_ data: Data, name: String, generation: UUID) {
        Task { [weak self] in
            do {
                let file = try await Self.writeAttachmentPreview(data, name: name)
                guard let self, self.attachmentPreviewGeneration == generation else {
                    try? FileManager.default.removeItem(at: file.directory)
                    return
                }
                if let current = self.previewTemporaryDirectory {
                    try? FileManager.default.removeItem(at: current)
                }
                self.previewTemporaryDirectory = file.directory
                self.previewURL = file.url
                self.isLoadingAttachmentPreview = false
            } catch {
                guard let self, self.attachmentPreviewGeneration == generation else { return }
                self.isLoadingAttachmentPreview = false
                self.showToast(error.localizedDescription, tone: .error)
            }
        }
    }

    private nonisolated static func writeAttachmentPreview(
        _ data: Data,
        name: String
    ) async throws -> AttachmentPreviewFile {
        try await Task.detached(priority: .userInitiated) {
            let directory = URL.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let candidateExtension = URL(fileURLWithPath: name).pathExtension
            let safeExtension = candidateExtension.utf8.count <= 16
                && candidateExtension.unicodeScalars.allSatisfy(CharacterSet.alphanumerics.contains)
                ? candidateExtension
                : ""
            let url = safeExtension.isEmpty
                ? directory.appending(path: "attachment")
                : directory.appending(path: "attachment").appendingPathExtension(safeExtension)
            #if os(iOS)
            try data.write(to: url, options: [.atomic, .completeFileProtection])
            #else
            try data.write(to: url, options: .atomic)
            #endif
            return AttachmentPreviewFile(directory: directory, url: url)
        }.value
    }

    private func widgets(in slot: FrontendSlot) -> [MountedWidget] {
        mountedWidgets.filter { $0.widget.slot == slot }
    }

    private func requestID(_ prefix: String) -> String {
        "\(prefix)-\(UUID().uuidString.lowercased())"
    }

    private func restoreDraft(id: String) {
        guard let draft = pendingDrafts.removeValue(forKey: id) else { return }
        restoreDraft(draft)
    }

    private func restoreDraft(_ draft: PendingComposerDraft) {
        if !draft.text.isEmpty {
            composer = composer.isEmpty ? draft.text : "\(draft.text)\n\n\(composer)"
        }
        let currentIDs = Set(composerAttachments.compactMap { item -> String? in
            guard case .uploaded(let attachment) = item.state else { return nil }
            return attachment.id
        })
        let available = max(0, maximumAttachmentReferences - composerAttachments.count)
        composerAttachments.insert(contentsOf: draft.attachments
            .filter { !currentIDs.contains($0.id) }
            .prefix(available)
            .map { attachment in
                ComposerAttachment(
                    id: UUID(),
                    name: attachment.name,
                    size: attachment.size,
                    mediaType: attachment.mediaType,
                    state: .uploaded(attachment)
                )
            }, at: 0)
    }

    private func restorePendingDrafts() {
        let drafts = pendingDrafts.keys.sorted().compactMap { pendingDrafts[$0] }
        pendingDrafts.removeAll()
        guard !drafts.isEmpty else { return }
        for draft in drafts.reversed() { restoreDraft(draft) }
    }

    private func connectionEnded(generation: UUID, message: String) {
        guard connectionGeneration == generation else { return }
        connectionState = .failed(message)
        attachmentUploadRequests.removeAll()
        activeAttachmentUpload = nil
        attachmentListRequestID = nil
        isLoadingAttachments = false
        gitDiffRequestID = nil
        isLoadingGitDiff = false
        workspaceFilesRequestID = nil
        isLoadingWorkspaceFiles = false
        discardPendingComposerAttachments()
        discardAttachmentPreview()
        restorePendingDrafts()
        if pendingPairingAccount != nil { pairingError = message }
        showToast(message, tone: .error)
    }

    @discardableResult
    private func resetGatewayState(
        preservingDrafts: Bool,
        preservingSession: Bool = false
    ) -> UUID {
        if preservingSession { flushStreamDeltas() }
        connectionGeneration = UUID()
        eventTask?.cancel()
        eventTask = nil
        if !preservingSession {
            latestSequence = nil
            currentReplayEpoch = nil
        }
        sessionOpenCursor = nil
        replayRequestID = nil
        replaySnapshotSequence = nil
        if !preservingSession { replayPresentedTranscript = nil }
        if preservingDrafts {
            discardPendingComposerAttachments()
        } else {
            pendingDrafts.removeAll()
            composer = ""
            discardComposerAttachments()
        }
        pendingPairingAccount = nil
        connectionState = .disconnected
        dismissToast()
        sessionRequestID = nil
        sessionOpeningID = nil
        pendingCachedTranscript = nil
        pendingPresentedTranscript = nil
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
        if preservingSession {
            gitDiffRequestID = nil
            isLoadingGitDiff = false
            workspaceFilesRequestID = nil
            isLoadingWorkspaceFiles = false
            attachmentListRequestID = nil
            isLoadingAttachments = false
            attachmentUploadRequests.removeAll()
            activeAttachmentUpload = nil
            discardAttachmentPreview()
        }
        if !preservingSession {
            sessions = []
            selectedSessionID = nil
            unreadSessionIDs.removeAll()
            profile = nil
            modelChoices = []
            modelProviders = [:]
            middlewareFeatures = []
            providerStatuses = []
            defaultAgentSnapshot = nil
            setupProviderDraft = nil
        }
        providerAPIKey = ""
        providerModelIDsText = ""
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
        if preservingDrafts { restorePendingDrafts() }
        return connectionGeneration
    }

    private func resetSessionState() {
        workspace = nil
        gitStatus = nil
        gitDiff = ""
        gitDiffRequestID = nil
        isLoadingGitDiff = false
        workspaceFiles = []
        workspaceFilesRequestID = nil
        isLoadingWorkspaceFiles = false
        workspaceViewerScope = .unstaged
        inspectorPage = .changes
        gitBranchRequestID = nil
        discardComposerAttachments()
        uploadedAttachments = []
        attachmentListRequestID = nil
        isLoadingAttachments = false
        attachmentUploadRequests.removeAll()
        activeAttachmentUpload = nil
        discardAttachmentPreview()
        selectedModelRoute = ""
        contributions = []
        toolCount = 0
        agentSnapshot = nil
        agentDraft = defaultAgentSnapshot?.config
        applyState = .idle
        configRequestID = nil
        cronTasks = []
        cronRuns = []
        cronTaskDraft = ""
        cronError = nil
        cronRequestIDs.removeAll()
        transcript = []
        deltaFlushTask?.cancel()
        deltaFlushTask = nil
        bufferedDeltas.removeAll()
        replayRequestID = nil
        replaySnapshotSequence = nil
        replayPresentedTranscript = nil
        activeTurnID = nil
        activeOperation = nil
        runStats = RunStats()
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
