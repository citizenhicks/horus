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
    case uploaded(SessionFileReference)
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
    let attachments: [SessionFileReference]
}

private struct PendingWidgetEdit {
    let owner: ComposerDraftOwner
    var recovery: ComposerEditRecovery
}

private struct ComposerDraftOwner: Equatable, Sendable {
    let accountID: UUID
    let sessionID: String
}

private struct ReplayUserMessage {
    let sequence: UInt64
    let text: String
}

private let maximumObservedReplaySubmissions = 1_024

private enum SessionFileUploadRequest {
    case begin(localID: UUID)
    case chunk(localID: UUID, expectedNextOffset: Int64)
    case finish(localID: UUID)

    var localID: UUID {
        switch self {
        case .begin(let localID), .chunk(let localID, _), .finish(let localID): localID
        }
    }
}

private struct ActiveSessionFileUpload {
    let localID: UUID
    let sessionID: String
    let uploadID: String
    let maxChunkBytes: Int
}

private struct SessionFileDownload {
    let generation: UUID
    let file: SessionFileReference
    let sessionID: String
    let purpose: SessionFileDownloadPurpose
    var data: Data
    var requestID: String
}

private enum SessionFileDownloadPurpose: Equatable {
    case preview
    case share
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

private struct TemporarySessionFile: Sendable {
    let directory: URL
    let url: URL
}

struct TextFilePreview: Identifiable {
    let id: UUID
    let name: String
    let contents: String
}

struct SessionFileShareItem: Identifiable {
    let id: UUID
    let name: String
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

enum FilesInspectorTab: String, CaseIterable, Identifiable {
    case unstaged
    case allFiles
    case chatFiles

    var id: Self { self }
}

extension SessionRecord {
    static let untitledDisplayTitle = "new conversation"

    var explicitTitle: String? {
        guard let title = title?.trimmingCharacters(in: .whitespacesAndNewlines),
              !title.isEmpty
        else { return nil }
        return title
    }

    var displayTitle: String {
        explicitTitle
            ?? ChatTitleWriter.preview(for: firstUserMessage)
            ?? Self.untitledDisplayTitle
    }
}

/// One entry in the workspace file tree. `children` is nil for a file, which is how
/// `List(children:)` decides a row gets no disclosure control.
struct FileTreeNode: Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let size: Int64?
    let children: [FileTreeNode]?

    var isFolder: Bool { children != nil }

    /// The gateway sends a flat list of paths; a browser needs them nested, folders first
    /// and then in the case-insensitive order Finder uses.
    static func tree(from files: [WorkspaceFileRecord]) -> [FileTreeNode] {
        nodes(
            files.map {
                (components: $0.path.split(separator: "/").map(String.init), size: Int64(clamping: $0.size))
            },
            prefix: ""
        )
    }

    private static func nodes(
        _ entries: [(components: [String], size: Int64)],
        prefix: String
    ) -> [FileTreeNode] {
        let groups = Dictionary(grouping: entries.filter { !$0.components.isEmpty }) {
            $0.components[0]
        }
        return groups.map { name, group -> FileTreeNode in
            let path = prefix.isEmpty ? name : "\(prefix)/\(name)"
            let nested = group
                .filter { $0.components.count > 1 }
                .map { (components: Array($0.components.dropFirst()), size: $0.size) }
            guard nested.isEmpty else {
                return FileTreeNode(id: path, name: name, size: nil, children: nodes(nested, prefix: path))
            }
            return FileTreeNode(id: path, name: name, size: group[0].size, children: nil)
        }
        .sorted {
            $0.isFolder == $1.isFolder
                ? $0.name.localizedStandardCompare($1.name) == .orderedAscending
                : $0.isFolder
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
        case .unavailable: "Require Face ID or Touch ID"
        }
    }

    var unlockTitle: String {
        switch self {
        case .faceID: "Unlock with Face ID"
        case .touchID: "Unlock with Touch ID"
        case .biometrics: "Unlock with Biometrics"
        case .unavailable: "Unlock with Face ID or Touch ID"
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
private let sharesHorusDiagnosticsKey = "shares-horus-diagnostics"
private let maximumAttachmentBytes = 25 * 1024 * 1024
private let maximumComposerAttachmentBytes: Int64 = 100 * 1024 * 1024
private let maximumPresentedFileBytes = 25 * 1024 * 1024
private let maximumHighlightedPreviewBytes = 1024 * 1024

@Observable
final class TranscriptEntry: Identifiable {
    enum Kind: String, Codable, Sendable {
        case user
        case assistant
        case commentary
        case reasoning
        case event
        case error
    }

    let id: String
    var text: String
    var kind: Kind
    var capability: String?
    var role: FrontendBlockRole?
    var update: FrontendBlockUpdate?
    var title: String
    var symbol: String?
    var group: String?
    var format: String
    var tone: String
    var pending: Bool
    var modelStepID: String?
    var sourceSequence: UInt64?
    var recordedAtMs: Int64?
    var messageTarget: MessageTarget?
    var files: [SessionFileReference]

    init(
        id: String,
        text: String,
        kind: Kind,
        capability: String? = nil,
        role: FrontendBlockRole? = nil,
        update: FrontendBlockUpdate? = nil,
        title: String = "",
        symbol: String? = nil,
        group: String? = nil,
        format: String,
        tone: String = "neutral",
        pending: Bool,
        modelStepID: String? = nil,
        sourceSequence: UInt64? = nil,
        recordedAtMs: Int64? = nil,
        messageTarget: MessageTarget? = nil,
        files: [SessionFileReference] = []
    ) {
        self.id = id
        self.text = text
        self.kind = kind
        self.capability = capability
        self.role = role
        self.update = update
        self.title = title
        self.symbol = symbol
        self.group = group
        self.format = format
        self.tone = tone
        self.pending = pending
        self.modelStepID = modelStepID
        self.sourceSequence = sourceSequence
        self.recordedAtMs = recordedAtMs
        self.messageTarget = messageTarget
        self.files = files
    }
}

/// Typed transcript presentation supplied by the framework.
extension TranscriptEntry {
    var headline: String { title }

    /// Everything under the heading — the tool output the one-line row hides.
    var eventDetail: String {
        text
    }

    /// Hosted web search is identified by its protocol role, independent of title or owner.
    var isWebSearch: Bool {
        role == .webSearch
    }

    /// Consecutive framework activity shares one visual row. Capability and protocol group
    /// still describe each entry, but neither interrupts a continuous sequence for the reader.
    static func groupedRows(
        from entries: [TranscriptEntry],
        breakBefore boundaryID: String? = nil
    ) -> [[TranscriptEntry]] {
        var rows: [[TranscriptEntry]] = []
        var activity: [TranscriptEntry] = []

        func flushActivity() {
            guard !activity.isEmpty else { return }
            rows.append(activity)
            activity = []
        }

        for entry in entries {
            if let boundaryID, entry.id == boundaryID {
                flushActivity()
            }

            let hasActivityRole = switch entry.role {
            case .activity, .tool, .webSearch: true
            case .artifact, .approval, .notice, nil: false
            }
            if (entry.kind == .event || entry.kind == .error) && hasActivityRole {
                activity.append(entry)
            } else {
                flushActivity()
                rows.append([entry])
            }
        }
        flushActivity()
        return rows
    }

    /// "3 tool calls • 4 web searches • 2 events • 1 error", skipping the empty categories.
    static func summary(for entries: [TranscriptEntry]) -> String {
        var tools = 0
        var searches = 0
        var events = 0
        var errors = 0
        for entry in entries {
            if entry.kind == .error || entry.tone == "error" {
                errors += 1
            } else if entry.isWebSearch {
                searches += 1
            } else if entry.role == .tool {
                tools += 1
            } else {
                events += 1
            }
        }
        return [(tools, "tool call"), (searches, "web search"), (events, "event"), (errors, "error")]
            .filter { $0.0 > 0 }
            .map { counted($0.0, $0.1) }
            .joined(separator: " • ")
    }

    private static func counted(_ count: Int, _ noun: String) -> String {
        guard count != 1 else { return "1 \(noun)" }
        let sibilant = ["ch", "sh", "s", "x"].contains { noun.hasSuffix($0) }
        return "\(count) \(noun)\(sibilant ? "es" : "s")"
    }
}

/// What makes a set of grouped transcript rows stale. Structure only: text deltas mutate
/// entries in place and leave the grouping alone.
private struct TranscriptRowsKey: Equatable {
    let version: Int
    let count: Int
    let boundaryID: String?
    let firstID: String?
    let lastID: String?
}

private struct BufferedAgentEvent {
    let record: RecordedEvent
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
    let replacement: String

    init(
        capability: String,
        reference: FrontendReference,
        replacement: String? = nil
    ) {
        self.capability = capability
        self.reference = reference
        self.replacement = replacement ?? "\(reference.trigger)\(reference.value)"
    }

    var id: String { "\(capability)\u{0}\(reference.trigger)\u{0}\(reference.value)" }
    var label: String { "\(reference.trigger)\(reference.value)" }
}

private enum ConfigurationTarget {
    case session
    case defaultAgent
}

struct ReferenceSuggestions: Sendable {
    let source: String
    let range: Range<String.Index>
    let matches: [MountedReference]
}

private struct ReferenceMatchScore: Comparable {
    let tier: Int
    let gaps: Int
    let length: Int

    static func < (lhs: Self, rhs: Self) -> Bool {
        if lhs.tier != rhs.tier { return lhs.tier < rhs.tier }
        if lhs.gaps != rhs.gaps { return lhs.gaps < rhs.gaps }
        return lhs.length < rhs.length
    }
}

struct TranscriptPreview: Identifiable {
    let id: String
    let title: String
    let context: String
    let status: String?
    let model: String?
    let entries: [TranscriptEntry]
    let next: AgentOperation?
}

struct FrontendPickerPrompt: Sendable {
    let title: String
    let options: [FrontendPickerOption]
}

private struct ChatTitleAttempt: Hashable {
    let accountID: UUID
    let sessionID: String
    let submissionID: String
    let prompt: String
}

private struct PendingChatTitle {
    let attempt: ChatTitleAttempt
    let previewTitle: String
    var generatedTitle: String?
    var renameRequestID: String?
    var submissionConfirmed: Bool

    var displayTitle: String { generatedTitle ?? previewTitle }
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
    private(set) var gitDiffRevision = 0
    var gitDiff = "" {
        didSet { gitDiffRevision &+= 1 }
    }
    var sessions: [SessionRecord] = []
    /// Whether a Horus Cloud account is connected.
    ///
    /// Hard-coded until the cloud sign-in exists. Account-gated rows read this instead of
    /// each deciding for itself, so connecting the cloud is one value to change. Horus is a
    /// free app with an optional cloud, so the signed-out state hides account rows rather
    /// than showing them disabled: a control nobody can enable from here teaches nothing.
    var hasCloudAccount: Bool { false }

    var gatewayMachineName = ""
    @ObservationIgnored private let titleWriter: ChatTitleWriter
    @ObservationIgnored private var chatTitleTasks: [String: Task<Void, Never>] = [:]
    @ObservationIgnored private var titleEligibleSessionIDs: Set<String> = []
    private var pendingChatTitles: [String: PendingChatTitle] = [:]
    var selectedSessionID: String?
    var sessionToRename: SessionRecord?
    var sessionRenameDraft = ""
    var sessionToDelete: SessionRecord?
    private(set) var unreadSessionIDs: Set<String> = []
    var transcript: [TranscriptEntry] = [] {
        didSet { invalidateTranscriptRows() }
    }
    private var replayPresentedTranscript: [TranscriptEntry]? {
        didSet { invalidateTranscriptRows() }
    }
    private var visibleTranscriptLimit = 300
    var displayedTranscript: [TranscriptEntry] {
        let source = replayPresentedTranscript ?? transcript
        return source.count > visibleTranscriptLimit
            ? Array(source.suffix(visibleTranscriptLimit))
            : source
    }
    /// The one visible activity label that represents the live turn's current step.
    var activeTranscriptStepID: String? {
        guard activeTurnID != nil,
              let latest = displayedTranscript.last,
              latest.pending,
              [.reasoning, .event, .error].contains(latest.kind)
        else { return nil }
        return latest.id
    }
    /// The turn is running with nothing pending, so no row is shimmering and the transcript
    /// would otherwise sit still while the model decides what to do next.
    var isWaitingForModel: Bool {
        TranscriptWaitingNote.isWaiting(
            hasActiveTurn: activeTurnID != nil,
            lastEntryIsPending: displayedTranscript.last?.pending == true,
            connectionIsReady: connectionState.isReady,
            hasPendingApproval: pendingApproval != nil,
            hasPendingPicker: pendingPicker != nil
        )
    }
    var isLoadingTranscript: Bool {
        guard connectionState == .loading,
              sessionRequestID != nil || replayRequestID != nil
        else { return false }

        let opensAnotherSession = sessionOpeningID.map { $0 != selectedSessionID } ?? false
        return opensAnotherSession || (replayPresentedTranscript ?? transcript).isEmpty
    }
    private(set) var isLoadingEarlierHistory = false
    var hasEarlierHistory: Bool {
        let source = replayPresentedTranscript ?? transcript
        return source.count > visibleTranscriptLimit
            || nextHistoryBeforeSequence != nil
            || isLoadingEarlierHistory
    }
    var canLoadEarlierHistory: Bool {
        hasEarlierHistory
            && connectionState.isReady
            && activeTurnID == nil
            && pendingApproval == nil
            && historyRequestID == nil
    }
    var composer = "" {
        didSet { scheduleComposerDraftSave() }
    }
    @ObservationIgnored private var transcriptRowsCache: (key: TranscriptRowsKey, rows: [[TranscriptEntry]])?
    @ObservationIgnored private var transcriptRowsVersion = 0
    private(set) var composerFocusRequest = 0
    /// Counterpart to `composerFocusRequest`: the composer owns the focus state, so anything
    /// outside it that needs the keyboard gone asks rather than reaching in.
    private(set) var composerBlurRequest = 0
    var composerAttachments: [ComposerAttachment] = []
    var sessionUploads: [SessionFileReference] = []
    private(set) var isLoadingSessionUploads = false
    var artifacts: [ArtifactRecord] = []
    private(set) var artifactsTruncated = false
    private(set) var isLoadingArtifacts = false
    var previewURL: URL?
    var textFilePreview: TextFilePreview?
    var sessionFileShareItem: SessionFileShareItem?
    private(set) var isLoadingFilePresentation = false
    var toast: AppToast?
    var activeTurnID: String?
    var activeOperation: String?
    private(set) var steeringDeliveryRevision = 0
    var contextTokens = 0
    var modelContextWindow: Int64?
    var pendingApproval: PendingApproval?
    var modelChoices: [ModelChoice] = []
    var modelProviders: [String: String] = [:]
    var middlewareFeatures: [MiddlewareFeature] = []
    var selectedModelRoute = ""
    private(set) var contributionsRevision = 0
    var contributions: [FrontendContribution] = [] {
        didSet { contributionsRevision &+= 1 }
    }
    var mountedWidgets: [MountedWidget] = []
    var pendingPicker: FrontendPickerPrompt?
    var previews: [TranscriptPreview] = []
    var presentedPreview: TranscriptPreview?
    private(set) var isLoadingPreviewPage = false
    var showsInspector = false
    var filesInspectorTab: FilesInspectorTab = .unstaged
    private(set) var workspaceFilesRevision = 0
    var workspaceFiles: [WorkspaceFileRecord] = [] {
        didSet { workspaceFilesRevision &+= 1 }
    }
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
    var defaultAgentDraft: AgentComposition?
    private var setupProviderDraft: ProviderConfig?
    var chatAgentApplyState: ApplyState = .idle
    var defaultAgentApplyState: ApplyState = .idle
    var providerStatuses: [ProviderStatus] = []
    var providerAPIKey = ""
    var providerModelIDsText = ""
    var providerReasoningEffortsText = ""
    var providerActionState: ProviderActionState = .idle
    var pairingCodeInfo: PairingCodeInfo?

    var showsPairing = false
    var pairingEndpoint = "wss://"
    var pairingCode = ""
    var pairingError: String?
    var theme: ThemePreference
    private(set) var sharesHorusDiagnostics: Bool
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
    @ObservationIgnored private let connectionOpener:
        @MainActor @Sendable (GatewayEndpoint) async throws -> AsyncThrowingStream<GatewayEnvelope, Error>
    @ObservationIgnored private let reconnectDelay: @Sendable (Int) -> Duration
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    @ObservationIgnored private var reconnectTask: Task<Void, Never>?
    @ObservationIgnored private var reconnectAttempt = 0
    @ObservationIgnored private var automaticReconnectBlocked = false
    @ObservationIgnored private var deltaFlushTask: Task<Void, Never>?
    @ObservationIgnored private var bufferedDeltas:
        [(
            id: String,
            delta: String,
            kind: TranscriptEntry.Kind,
            modelStepID: String,
            sourceSequence: UInt64,
            recordedAtMs: Int64
        )] = []
    @ObservationIgnored private var connectionGeneration = UUID()
    @ObservationIgnored private var reconnectsOnActivation = false
    @ObservationIgnored private var pendingPairingAccount: GatewayAccount?
    @ObservationIgnored private var pendingDrafts: [String: PendingComposerDraft] = [:]
    private var pendingWidgetEdit: PendingWidgetEdit?
    private var stashedComposerDraft: String?
    private var isLoadingComposerEditRecovery = false
    @ObservationIgnored private var composerEditRecoveryGeneration = UUID()
    @ObservationIgnored private var replayCompletionSubmissionIDs: Set<String> = []
    @ObservationIgnored private var replayUserMessages: [ReplayUserMessage] = []
    @ObservationIgnored private var completedComposerEditReplay = false
    @ObservationIgnored private var awaitsSteeringDelivery = false
    @ObservationIgnored private var composerDraftOwner: ComposerDraftOwner?
    @ObservationIgnored private var composerDraftGeneration = UUID()
    @ObservationIgnored private var composerDraftSaveTask: Task<Void, Never>?
    @ObservationIgnored private var composerDraftIOTask: Task<Void, Never>?
    @ObservationIgnored private var isLoadingComposerDraft = false
    @ObservationIgnored private var suppressesComposerDraftSave = false
    @ObservationIgnored private var transcriptIOTask: Task<Void, Never>?
    @ObservationIgnored private var transcriptLoadGeneration = UUID()
    @ObservationIgnored private var sessionRequestID: String?
    @ObservationIgnored private var sessionOpeningID: String?
    @ObservationIgnored private var pendingCachedTranscript: CachedTranscript?
    @ObservationIgnored private var pendingPresentedTranscript: [TranscriptEntry]?
    private var sessionMutationRequestID: String?
    @ObservationIgnored private var pendingDeletedSessionID: String?
    @ObservationIgnored private var sessionToRestoreID: String?
    @ObservationIgnored private var configRequestID: String?
    @ObservationIgnored private var defaultConfigRequestID: String?
    @ObservationIgnored private var submittedDefaultAgentDraft: AgentComposition?
    @ObservationIgnored private var approvalRequestID: String?
    @ObservationIgnored private var directoryRequestID: String?
    @ObservationIgnored private var gitDiffRequestID: String?
    @ObservationIgnored private var workspaceFilesRequestID: String?
    @ObservationIgnored private var sessionUploadsRequestID: String?
    @ObservationIgnored private var artifactListRequestID: String?
    @ObservationIgnored private var sessionFileUploadRequests: [String: SessionFileUploadRequest] = [:]
    @ObservationIgnored private var sessionFileData: [UUID: Data] = [:]
    @ObservationIgnored private var attachmentImportReservations = 0
    @ObservationIgnored private var attachmentImportGeneration = UUID()
    @ObservationIgnored private var activeSessionFileUpload: ActiveSessionFileUpload?
    @ObservationIgnored private var sessionFileDownload: SessionFileDownload?
    @ObservationIgnored private var workspaceFilePreviewDownload: WorkspaceFilePreviewDownload?
    @ObservationIgnored private var filePresentationGeneration = UUID()
    @ObservationIgnored private var previewTemporaryDirectory: URL?
    private var gitBranchRequestID: String?
    @ObservationIgnored private var credentialRequestID: String?
    @ObservationIgnored private var pairingCodeRequestID: String?
    @ObservationIgnored private var pairingCodeExpiryTask: Task<Void, Never>?
    @ObservationIgnored private var providerLoginRequestID: String?
    @ObservationIgnored private var providerRegistrationRequestID: String?
    @ObservationIgnored private var cronRequestIDs: Set<String> = []
    @ObservationIgnored private var toastDismissTask: Task<Void, Never>?
    @ObservationIgnored private var isChatVisible = false
    @ObservationIgnored private var latestSequence: UInt64?
    @ObservationIgnored private var sessionOpenCursor: UInt64?
    @ObservationIgnored private var replayRequestID: String?
    @ObservationIgnored private var replaySnapshotSequence: UInt64?
    @ObservationIgnored private var transcriptRecordBase: [TranscriptEntry] = []
    @ObservationIgnored private var transcriptRecordBaseSequence: UInt64?
    @ObservationIgnored private var transcriptRecords: [UInt64: RecordedEvent] = [:]
    @ObservationIgnored private var historyRequestID: String?
    @ObservationIgnored private var nextHistoryBeforeSequence: UInt64?
    @ObservationIgnored private var previewSelections: [String: FrontendPickerOption] = [:]
    @ObservationIgnored private var previewPageRequestID: String?
    @ObservationIgnored private var appIsInBackground = true

    init(
        client: GatewayClient? = nil,
        store: GatewayStore? = nil,
        settingsDefaults: UserDefaults = .standard,
        appLockAuthenticator: AppLockAuthenticator? = nil,
        requestSender: (@MainActor @Sendable (GatewayRequest) async throws -> Void)? = nil,
        connectionOpener: (
            @MainActor @Sendable (GatewayEndpoint) async throws
                -> AsyncThrowingStream<GatewayEnvelope, Error>
        )? = nil,
        reconnectDelay: (@Sendable (Int) -> Duration)? = nil,
        titleWriter: ChatTitleWriter? = nil
    ) {
        let client = client ?? GatewayClient()
        let store = store ?? GatewayStore()
        let appLockAuthenticator = appLockAuthenticator ?? AppLockAuthenticator()
        let appLockEnabled = settingsDefaults.bool(forKey: appLockEnabledKey)
        self.client = client
        self.store = store
        self.settingsDefaults = settingsDefaults
        self.appLockAuthenticator = appLockAuthenticator
        self.titleWriter = titleWriter ?? ChatTitleWriter()
        self.requestSender = requestSender ?? { request in
            try await client.send(request)
        }
        self.connectionOpener = connectionOpener ?? { endpoint in
            try await client.connect(to: endpoint)
        }
        self.reconnectDelay = reconnectDelay ?? { attempt in
            let seconds = min(
                8,
                0.5 * pow(2, Double(min(attempt, 4))) * Double.random(in: 0.75...1.25)
            )
            return .milliseconds(Int64(seconds * 1_000))
        }
        self.accounts = store.loadAccounts()
        self.selectedAccountID = store.selectedAccountID()
        self.theme = ThemePreference(rawValue: settingsDefaults.string(forKey: "theme") ?? "") ?? .system
        self.sharesHorusDiagnostics = settingsDefaults.bool(forKey: sharesHorusDiagnosticsKey)
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
        reconnectTask?.cancel()
        deltaFlushTask?.cancel()
        composerDraftSaveTask?.cancel()
        pairingCodeExpiryTask?.cancel()
        toastDismissTask?.cancel()
        chatTitleTasks.values.forEach { $0.cancel() }
    }

    var selectedAccount: GatewayAccount? {
        accounts.first { $0.id == selectedAccountID }
    }

    var canOpenSession: Bool {
        connectionState.isReady
            && pendingDrafts.isEmpty
            && sessionRequestID == nil
            && sessionMutationRequestID == nil
            && gitBranchRequestID == nil
            && sessionFileUploadRequests.isEmpty
            && pendingWidgetEdit == nil
            && !isLoadingComposerEditRecovery
            && !isApplyingConfiguration
    }

    var canCreateSession: Bool { canOpenSession }

    var canRenameSession: Bool {
        connectionState.isReady && sessionMutationRequestID == nil
    }

    var canModifySelectedSession: Bool {
        canOpenSession && activeTurnID == nil && pendingApproval == nil
    }

    func isCapabilityEnabled(_ capability: String) -> Bool {
        guard let snapshot = agentSnapshot else { return false }
        guard let feature = middlewareFeatures.first(where: { $0.id == capability }) else {
            return snapshot.config.middleware.enabled.contains(capability)
                || contributions.contains { $0.capability == capability }
        }
        return feature.required
            || snapshot.config.middleware.enabled.contains(capability)
    }

    var isSchedulingEnabled: Bool { isCapabilityEnabled("cron") }

    var canStartCronSetup: Bool {
        canModifySelectedSession
            && selectedSessionID != nil
            && isSchedulingEnabled
    }

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
            && pendingWidgetEdit == nil
    }

    var canSendComposer: Bool {
        guard connectionState.isReady,
              let sessionID = selectedSessionID,
              sessionRequestID == nil,
              !isLoadingComposerDraft,
              !isLoadingComposerEditRecovery
        else { return false }
        if let pending = pendingWidgetEdit {
            guard let accountID = selectedAccountID,
                  pending.owner == ComposerDraftOwner(accountID: accountID, sessionID: sessionID),
                  pending.recovery.phase == .editing
            else { return false }
        }
        let hasText = !composer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let uploaded = uploadedComposerAttachments
        guard uploaded.isEmpty || canSubmitAttachments else { return false }
        return hasText || !uploaded.isEmpty
    }

    private var uploadedComposerAttachments: [SessionFileReference] {
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
            || chatAgentApplyState == .applying
            || chatAgentApplyState == .restarting
            || defaultAgentApplyState == .applying
            || defaultAgentApplyState == .restarting
    }

    var providerDraft: ProviderConfig? {
        get { defaultAgentDraft?.provider ?? setupProviderDraft }
        set {
            guard let newValue else {
                setupProviderDraft = nil
                return
            }
            if defaultAgentDraft != nil {
                defaultAgentDraft?.provider = newValue
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

    /// Asks the composer to give up the keyboard. Leaving it up while the drawer slides means
    /// the page animates against a keyboard that belongs to a screen the reader just left.
    func dismissComposerFocus() {
        composerBlurRequest &+= 1
    }

    /// Grouped transcript rows, computed once per structural change rather than per frame.
    ///
    /// The transcript view rebuilds many times a second while a turn streams, and grouping
    /// allocated a fresh array per row on every pass. Text deltas mutate entries in place and
    /// never change the grouping, so the key tracks structure only: the count and the end ids
    /// catch appends, replacements and session switches, and `transcriptRowsVersion` covers
    /// the in-place kind and role changes those cannot see.
    func transcriptRows(breakBefore boundaryID: String?) -> [[TranscriptEntry]] {
        let source = displayedTranscript
        let key = TranscriptRowsKey(
            version: transcriptRowsVersion,
            count: source.count,
            boundaryID: boundaryID,
            firstID: source.first?.id,
            lastID: source.last?.id
        )
        if let cached = transcriptRowsCache, cached.key == key { return cached.rows }
        let rows = TranscriptEntry.groupedRows(from: source, breakBefore: boundaryID)
        transcriptRowsCache = (key, rows)
        return rows
    }

    private func invalidateTranscriptRows() {
        transcriptRowsVersion &+= 1
    }

    /// Starts the on-device rewrite with the submitted first message. The task is stored,
    /// but deliberately not awaited, so the gateway turn and Foundation Models run together.
    private func startChatTitle(
        prompt submittedPrompt: String,
        submissionID: String,
        sessionID: String
    ) {
        let prompt = submittedPrompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let previewTitle = ChatTitleWriter.preview(for: prompt),
              titleEligibleSessionIDs.contains(sessionID),
              let accountID = selectedAccountID
        else { return }
        guard sessions.first(where: { $0.sessionId == sessionID })?.explicitTitle == nil
        else {
            titleEligibleSessionIDs.remove(sessionID)
            return
        }
        let attempt = ChatTitleAttempt(
            accountID: accountID,
            sessionID: sessionID,
            submissionID: submissionID,
            prompt: prompt
        )
        pendingChatTitles[sessionID] = PendingChatTitle(
            attempt: attempt,
            previewTitle: previewTitle,
            generatedTitle: nil,
            renameRequestID: nil,
            submissionConfirmed: false
        )
        titleEligibleSessionIDs.remove(sessionID)
        let titleWriter = titleWriter
        chatTitleTasks[sessionID] = Task { [weak self] in
            let outcome = await titleWriter.title(for: prompt) { [weak self] message in
                self?.showToast(message, tone: .warning)
            }
            guard let self else { return }
            self.finishChatTitle(outcome, attempt: attempt)
        }
    }

    private func finishChatTitle(_ outcome: ChatTitleWriter.Outcome, attempt: ChatTitleAttempt) {
        guard pendingChatTitles[attempt.sessionID]?.attempt == attempt else { return }
        chatTitleTasks.removeValue(forKey: attempt.sessionID)
        guard !Task.isCancelled, selectedAccountID == attempt.accountID
        else {
            pendingChatTitles.removeValue(forKey: attempt.sessionID)
            return
        }
        switch outcome {
        case .title(let title):
            pendingChatTitles[attempt.sessionID]?.generatedTitle = title
        case .failed(let message):
            showToast(message, tone: .warning)
        case .cancelled:
            break
        }
        reconcileChatTitles()
    }

    private func confirmChatTitle(submissionID: String) {
        guard let sessionID = pendingChatTitles.first(where: {
            $0.value.attempt.submissionID == submissionID
        })?.key else { return }
        confirmChatTitle(sessionID: sessionID)
    }

    private func confirmChatTitle(sessionID: String) {
        guard pendingChatTitles[sessionID] != nil else { return }
        pendingChatTitles[sessionID]?.submissionConfirmed = true
        persistGeneratedChatTitles()
    }

    private func reconcileChatTitles() {
        for sessionID in Array(pendingChatTitles.keys) {
            guard let pending = pendingChatTitles[sessionID] else { continue }
            guard pending.attempt.accountID == selectedAccountID else {
                cancelChatTitle(sessionID)
                continue
            }
            guard let session = sessions.first(where: { $0.sessionId == sessionID }) else {
                continue
            }
            if let durableTitle = session.explicitTitle {
                if durableTitle == pending.generatedTitle {
                    completeChatTitle(sessionID)
                } else {
                    // An explicit user or another client always wins.
                    cancelChatTitle(sessionID)
                }
                continue
            }

            let catalogPrompt = (session.firstUserMessage ?? "")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if !catalogPrompt.isEmpty {
                guard pending.attempt.prompt.hasPrefix(catalogPrompt) else {
                    cancelChatTitle(sessionID)
                    continue
                }
                if !pending.submissionConfirmed {
                    pendingChatTitles[sessionID]?.submissionConfirmed = true
                }
                if pending.generatedTitle == nil, chatTitleTasks[sessionID] == nil {
                    // The catalog now owns the same deterministic preview, so the temporary
                    // override is no longer needed.
                    completeChatTitle(sessionID)
                }
            } else if let requestID = pending.renameRequestID,
                      requestID != sessionMutationRequestID {
                // The mutation slot cleared without the generated title reaching the catalog.
                cancelChatTitle(sessionID)
            }
        }
        persistGeneratedChatTitles()
    }

    private func persistGeneratedChatTitles() {
        guard connectionState.isReady,
              sessionMutationRequestID == nil,
              let accountID = selectedAccountID
        else { return }

        for sessionID in pendingChatTitles.keys.sorted() {
            guard var pending = pendingChatTitles[sessionID],
                  pending.attempt.accountID == accountID,
                  let title = pending.generatedTitle,
                  pending.submissionConfirmed,
                  pending.renameRequestID == nil
            else { continue }
            if sessions.first(where: { $0.sessionId == sessionID })?.explicitTitle != nil {
                cancelChatTitle(sessionID)
                continue
            }
            guard let requestID = requestSessionRename(
                sessionID: sessionID,
                title: title,
                generatedTitleSessionID: sessionID
            ) else { return }
            pending.renameRequestID = requestID
            pendingChatTitles[sessionID] = pending
            return
        }
    }

    private func cancelChatTitle(_ sessionID: String, rearm: Bool = false) {
        chatTitleTasks.removeValue(forKey: sessionID)?.cancel()
        pendingChatTitles.removeValue(forKey: sessionID)
        if rearm { titleEligibleSessionIDs.insert(sessionID) }
    }

    private func cancelChatTitle(submissionID: String, rearm: Bool) {
        guard let sessionID = pendingChatTitles.first(where: {
            $0.value.attempt.submissionID == submissionID
        })?.key else { return }
        cancelChatTitle(sessionID, rearm: rearm)
    }

    private func completeChatTitle(_ sessionID: String) {
        chatTitleTasks.removeValue(forKey: sessionID)?.cancel()
        pendingChatTitles.removeValue(forKey: sessionID)
        titleEligibleSessionIDs.remove(sessionID)
    }

    private func prepareChatTitle(for sessionID: String) {
        cancelChatTitle(sessionID)
        titleEligibleSessionIDs.insert(sessionID)
    }

    var capabilityReferences: [MountedReference] {
        contributions.flatMap { contribution in
            contribution.references.map {
                MountedReference(capability: contribution.capability, reference: $0)
            }
        }
    }

    var currentSessionTitle: String {
        selectedSessionID.map(sessionTitle) ?? SessionRecord.untitledDisplayTitle
    }

    var selectedSession: SessionRecord? {
        guard let selectedSessionID else { return nil }
        return sessions.first { $0.sessionId == selectedSessionID }
    }

    func beginRenamingSession(_ session: SessionRecord) {
        sessionRenameDraft = displayedTitle(for: session)
        sessionToRename = session
    }

    func beginDeletingSession(_ session: SessionRecord) {
        sessionToDelete = session
    }

    func displayedTitle(for session: SessionRecord) -> String {
        pendingChatTitles[session.sessionId]?.displayTitle ?? session.displayTitle
    }

    private func sessionTitle(_ sessionID: String) -> String {
        if let pendingTitle = pendingChatTitles[sessionID]?.displayTitle {
            return pendingTitle
        }
        let session = sessions.first(where: { $0.sessionId == sessionID })
        return session.map { String($0.displayTitle.prefix(72)) }
            ?? SessionRecord.untitledDisplayTitle
    }

    var headerWidgets: [MountedWidget] { widgets(in: .header) }
    var transcriptTailWidgets: [MountedWidget] { widgets(in: .transcriptTail) }
    var composerHeaderWidgets: [MountedWidget] { widgets(in: .composerHeader) }
    var composerFooterWidgets: [MountedWidget] { widgets(in: .composerFooter) }
    var messageActionWidgets: [MountedWidget] {
        widgets(in: .messageActions).filter { $0.widget.action != nil }
    }
    var navigationWidgets: [MountedWidget] { widgets(in: .navigation) }
    var chatMenuWidgets: [MountedWidget] { widgets(in: .chatMenu) }

    func referenceSuggestions(in text: String, cursor: String.Index) -> ReferenceSuggestions? {
        guard text.indices.contains(cursor) || cursor == text.endIndex else { return nil }
        return Self.referenceSuggestions(
            in: text,
            cursorOffset: text.distance(from: text.startIndex, to: cursor),
            capabilityReferences: capabilityReferences,
            workspaceFiles: workspaceFiles
        )
    }

    nonisolated static func referenceSuggestions(
        in text: String,
        cursorOffset: Int,
        capabilityReferences: [MountedReference],
        workspaceFiles: [WorkspaceFileRecord]
    ) -> ReferenceSuggestions? {
        guard cursorOffset >= 0, cursorOffset <= text.count else { return nil }
        let cursor = text.index(text.startIndex, offsetBy: cursorOffset)
        let start = text[..<cursor].lastIndex(where: { $0.isWhitespace })
            .map { text.index(after: $0) } ?? text.startIndex
        guard start < cursor, let trigger = text[start..<cursor].first else { return nil }
        let end = text[cursor...].firstIndex(where: { $0.isWhitespace }) ?? text.endIndex
        let queryStart = text.index(after: start)
        let query = String(text[queryStart..<end]).lowercased()
        let capabilityMatches = capabilityReferences.filter { $0.reference.trigger == trigger }
        var matches: [MountedReference]

        if query.isEmpty {
            matches = Array(capabilityMatches.prefix(8))
            if trigger == "@", matches.count < 8 {
                matches.append(contentsOf: workspaceFiles.prefix(8 - matches.count).map {
                    Self.workspaceReference($0)
                })
            }
        } else {
            var ranked: [(score: ReferenceMatchScore, reference: MountedReference)] = []
            func consider(_ reference: MountedReference) {
                guard let score = referenceScore(reference.reference.value, query: query) else {
                    return
                }
                let index = ranked.firstIndex {
                    score < $0.score
                        || (score == $0.score
                            && reference.reference.value < $0.reference.reference.value)
                } ?? ranked.endIndex
                guard index < 8 else { return }
                ranked.insert((score, reference), at: index)
                if ranked.count > 8 { ranked.removeLast() }
            }
            capabilityMatches.forEach(consider)
            if trigger == "@" {
                workspaceFiles.lazy.map(Self.workspaceReference).forEach(consider)
            }
            matches = ranked.map { $0.reference }
        }
        guard !matches.isEmpty else { return nil }
        return ReferenceSuggestions(source: text, range: start..<end, matches: matches)
    }

    nonisolated private static func workspaceReference(
        _ file: WorkspaceFileRecord
    ) -> MountedReference {
        MountedReference(
            capability: "workspace-files",
            reference: FrontendReference(trigger: "@", value: file.path, description: "file"),
            replacement: file.path.contains(where: \Character.isWhitespace)
                && !file.path.contains("\"")
                ? "\"\(file.path)\""
                : file.path
        )
    }

    nonisolated private static func referenceScore(
        _ value: String,
        query: String
    ) -> ReferenceMatchScore? {
        let value = value.lowercased()
        let name = value.split(separator: "/").last.map(String.init) ?? value
        let length = value.count
        if name == query { return ReferenceMatchScore(tier: 0, gaps: 0, length: length) }
        if name.hasPrefix(query) { return ReferenceMatchScore(tier: 1, gaps: 0, length: length) }
        if value.hasPrefix(query) { return ReferenceMatchScore(tier: 2, gaps: 0, length: length) }
        if let range = name.range(of: query) {
            return ReferenceMatchScore(
                tier: 3,
                gaps: name.distance(from: name.startIndex, to: range.lowerBound),
                length: length
            )
        }
        if let range = value.range(of: query) {
            return ReferenceMatchScore(
                tier: 4,
                gaps: value.distance(from: value.startIndex, to: range.lowerBound),
                length: length
            )
        }
        if let gaps = subsequenceGaps(in: name, query: query) {
            return ReferenceMatchScore(tier: 5, gaps: gaps, length: length)
        }
        return subsequenceGaps(in: value, query: query).map {
            ReferenceMatchScore(tier: 6, gaps: $0, length: length)
        }
    }

    nonisolated private static func subsequenceGaps(in value: String, query: String) -> Int? {
        var searchStart = value.startIndex
        var firstOffset: Int?
        var lastOffset = 0
        var count = 0
        for wanted in query {
            guard let index = value[searchStart...].firstIndex(of: wanted) else { return nil }
            let offset = value.distance(from: value.startIndex, to: index)
            if firstOffset == nil { firstOffset = offset }
            lastOffset = offset
            count += 1
            searchStart = value.index(after: index)
        }
        return lastOffset + 1 - (firstOffset ?? 0) - count
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

    func handleOpenURL(_ url: URL) {
        applyPairingURL(url)
    }

    private func prefillPairing(_ parse: () throws -> GatewayPairingSetup) {
        cancelReconnect()
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
        cancelReconnect()
        automaticReconnectBlocked = false
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
                try await self.requestSender(.pair(
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
            cancelReconnect()
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
        cancelReconnect()
        let pendingDraftIO = composerDraftIOTask
        discardComposerDraft()
        Task { [weak self] in
            guard let self else { return }
            do {
                await pendingDraftIO?.value
                try await store.remove(account)
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
    }

    func openNewSession() {
        openWorkspaceBrowser()
    }

    func openNewSessionInCurrentWorkspace() {
        guard let path = workspace?.path else { return }
        chooseWorkspace(path)
    }

    func openSession(_ sessionID: String) {
        guard canOpenSession, sessionID != selectedSessionID else { return }
        let generation = UUID()
        transcriptLoadGeneration = generation
        let accountID = selectedAccountID
        let previous = transcriptIOTask
        transcriptIOTask = Task { [weak self, store] in
            await previous?.value
            let cached: CachedTranscript? = if let accountID {
                await store.loadTranscript(accountID: accountID, sessionID: sessionID)
            } else {
                nil
            }
            guard let self,
                  generation == transcriptLoadGeneration,
                  accountID == selectedAccountID,
                  canOpenSession,
                  sessionID != selectedSessionID
            else { return }
            requestSessionOpen(
                sessionID,
                lastSequence: cached?.sequence,
                cachedTranscript: cached,
                presentedTranscript: cached?.transcript
            )
        }
    }

    func loadEarlierHistory() {
        guard canLoadEarlierHistory else { return }
        let source = replayPresentedTranscript ?? transcript
        if source.count > visibleTranscriptLimit {
            visibleTranscriptLimit = min(source.count, visibleTranscriptLimit + 300)
            return
        }
        guard let sessionID = selectedSessionID,
              let beforeSequence = nextHistoryBeforeSequence
        else { return }
        let id = requestID("history")
        historyRequestID = id
        isLoadingEarlierHistory = true
        transmit(.getSessionHistory(
            requestID: id,
            sessionID: sessionID,
            beforeSequence: beforeSequence,
            maxEvents: 300
        )) { [weak self] _ in
            guard self?.historyRequestID == id else { return }
            self?.historyRequestID = nil
            self?.isLoadingEarlierHistory = false
        }
    }

    func restoreSession(_ sessionID: String) {
        flushStreamDeltas()
        guard sessionID == selectedSessionID,
              let sequence = latestSequence
        else {
            requestSessionOpen(sessionID, lastSequence: nil)
            return
        }
        let base = CachedTranscript(
            sequence: sequence,
            nextBeforeSequence: nextHistoryBeforeSequence,
            transcript: transcript,
            currentUsage: currentUsage,
            lastUsage: lastUsage
        )
        let presentation = CachedTranscript(
            sequence: sequence,
            nextBeforeSequence: nextHistoryBeforeSequence,
            transcript: displayedTranscript,
            currentUsage: currentUsage,
            lastUsage: lastUsage
        ).transcript
        requestSessionOpen(
            sessionID,
            lastSequence: sequence,
            cachedTranscript: base,
            presentedTranscript: presentation
        )
    }

    private func requestSessionOpen(
        _ sessionID: String,
        lastSequence: UInt64?,
        cachedTranscript: CachedTranscript? = nil,
        presentedTranscript: [TranscriptEntry]? = nil
    ) {
        replayCompletionSubmissionIDs.removeAll(keepingCapacity: true)
        replayUserMessages.removeAll(keepingCapacity: true)
        completedComposerEditReplay = false
        if sessionID != selectedSessionID {
            discardComposerAttachments()
            discardFilePresentation()
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
            lastSequence: lastSequence
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
    @discardableResult
    func renameSession(_ session: SessionRecord, title: String) -> String? {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return nil }
        guard sessionMutationRequestID == nil else {
            showToast("Another chat update is finishing.", tone: .info)
            return nil
        }
        cancelChatTitle(session.sessionId)
        return requestSessionRename(sessionID: session.sessionId, title: title)
    }

    @discardableResult
    private func requestSessionRename(
        sessionID: String,
        title: String,
        generatedTitleSessionID: String? = nil
    ) -> String? {
        guard sessionMutationRequestID == nil else { return nil }
        let id = requestID("session-rename")
        sessionMutationRequestID = id
        transmit(.renameSession(
            requestID: id,
            sessionID: sessionID,
            title: title
        )) { [weak self] _ in
            guard let self else { return }
            if self.sessionMutationRequestID == id { self.sessionMutationRequestID = nil }
            if let generatedTitleSessionID,
               self.pendingChatTitles[generatedTitleSessionID]?.renameRequestID == id {
                self.cancelChatTitle(generatedTitleSessionID)
            }
        }
        return id
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
            enqueueTranscriptIO { [store] in
                await store.removeTranscript(accountID: accountID, sessionID: session.sessionId)
            }
        }
        let id = requestID("session-delete")
        sessionMutationRequestID = id
        pendingDeletedSessionID = session.sessionId
        transmit(.deleteSession(
            requestID: id,
            sessionID: session.sessionId
        )) { [weak self] _ in
            guard self?.sessionMutationRequestID == id else { return }
            self?.sessionMutationRequestID = nil
            self?.pendingDeletedSessionID = nil
        }
    }

    private func refreshWorkspaceChanges() {
        refreshGitDiff()
        refreshWorkspaceFiles()
    }

    private func refreshGitDiff() {
        guard connectionState.isReady, let sessionID = selectedSessionID else { return }
        let id = requestID("git-diff")
        gitDiffRequestID = id
        isLoadingGitDiff = true
        transmit(.getGitDiff(requestID: id, sessionID: sessionID, scope: .unstaged)) { [weak self] _ in
            guard self?.gitDiffRequestID == id else { return }
            self?.gitDiffRequestID = nil
            self?.isLoadingGitDiff = false
        }
    }

    func selectFilesInspectorTab(_ tab: FilesInspectorTab) {
        guard filesInspectorTab != tab else { return }
        filesInspectorTab = tab
        refreshFiles(for: tab)
    }

    private func refreshWorkspaceFiles() {
        guard connectionState.isReady,
              let sessionID = selectedSessionID
        else { return }
        let id = requestID("workspace-files")
        workspaceFilesRequestID = id
        isLoadingWorkspaceFiles = true
        transmit(.listWorkspaceFiles(
            requestID: id,
            sessionID: sessionID,
            scope: .all
        )) { [weak self] _ in
            guard self?.workspaceFilesRequestID == id else { return }
            self?.workspaceFilesRequestID = nil
            self?.isLoadingWorkspaceFiles = false
        }
    }

    func switchGitBranch(to branch: String) {
        guard canModifySelectedSession,
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
            maximumSessionFileReferences - composerAttachments.count - attachmentImportReservations
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
                sessionFileData[id] = imported.data
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
        startNextSessionFileUpload()
    }

    func removeComposerAttachment(_ id: UUID) {
        guard activeSessionFileUpload?.localID != id else { return }
        sessionFileData[id] = nil
        composerAttachments.removeAll { $0.id == id }
    }

    func retryComposerAttachment(_ id: UUID) {
        guard sessionFileData[id] != nil,
              let index = composerAttachments.firstIndex(where: { $0.id == id }),
              case .failed = composerAttachments[index].state
        else { return }
        composerAttachments[index].state = .queued
        startNextSessionFileUpload()
    }

    func refreshSessionUploads() {
        guard connectionState.isReady, let sessionID = selectedSessionID else { return }
        let id = requestID("session-uploads")
        sessionUploadsRequestID = id
        isLoadingSessionUploads = true
        transmit(.listSessionUploads(requestID: id, sessionID: sessionID)) { [weak self] _ in
            guard self?.sessionUploadsRequestID == id else { return }
            self?.sessionUploadsRequestID = nil
            self?.isLoadingSessionUploads = false
        }
    }

    func refreshChatFiles() {
        refreshArtifacts()
        refreshSessionUploads()
    }

    private func refreshArtifacts() {
        guard connectionState.isReady, let sessionID = selectedSessionID else { return }
        let id = requestID("artifacts")
        artifactListRequestID = id
        isLoadingArtifacts = true
        transmit(.listArtifacts(requestID: id, sessionID: sessionID)) { [weak self] _ in
            guard self?.artifactListRequestID == id else { return }
            self?.artifactListRequestID = nil
            self?.isLoadingArtifacts = false
        }
    }

    func previewSessionFile(_ file: SessionFileReference) {
        downloadSessionFile(file, purpose: .preview)
    }

    func saveOrShareSessionFile(_ file: SessionFileReference) {
        downloadSessionFile(file, purpose: .share)
    }

    private func downloadSessionFile(
        _ file: SessionFileReference,
        purpose: SessionFileDownloadPurpose
    ) {
        guard let sessionID = selectedSessionID else { return }
        guard file.size <= Int64(maximumPresentedFileBytes) else {
            showToast("File downloads are limited to 25 MiB.", tone: .warning)
            return
        }
        discardFilePresentation()
        let id = requestID("session-file-read")
        let generation = UUID()
        filePresentationGeneration = generation
        sessionFileDownload = SessionFileDownload(
            generation: generation,
            file: file,
            sessionID: sessionID,
            purpose: purpose,
            data: Data(),
            requestID: id
        )
        isLoadingFilePresentation = true
        transmit(.readSessionFile(
            requestID: id,
            sessionID: sessionID,
            fileID: file.id,
            offset: 0,
            maxBytes: 256 * 1024
        )) { [weak self] message in
            guard self?.sessionFileDownload?.requestID == id else { return }
            self?.sessionFileDownload = nil
            self?.isLoadingFilePresentation = false
            self?.showToast(message, tone: .error)
        }
    }

    func previewWorkspaceFile(_ file: WorkspaceFileRecord) {
        guard let sessionID = selectedSessionID else { return }
        guard file.size <= UInt64(maximumPresentedFileBytes) else {
            showToast("Quick Look previews are limited to 25 MiB.", tone: .warning)
            return
        }
        discardFilePresentation()
        let id = requestID("workspace-file-read")
        let generation = UUID()
        filePresentationGeneration = generation
        workspaceFilePreviewDownload = WorkspaceFilePreviewDownload(
            generation: generation,
            file: file,
            sessionID: sessionID,
            data: Data(),
            requestID: id
        )
        isLoadingFilePresentation = true
        transmit(.readWorkspaceFile(
            requestID: id,
            sessionID: sessionID,
            path: file.path,
            offset: 0,
            maxBytes: 256 * 1024
        )) { [weak self] message in
            guard self?.workspaceFilePreviewDownload?.requestID == id else { return }
            self?.workspaceFilePreviewDownload = nil
            self?.isLoadingFilePresentation = false
            self?.showToast(message, tone: .error)
        }
    }

    func discardFilePresentation() {
        filePresentationGeneration = UUID()
        sessionFileDownload = nil
        workspaceFilePreviewDownload = nil
        isLoadingFilePresentation = false
        if let previewTemporaryDirectory {
            Task.detached(priority: .utility) {
                try? FileManager.default.removeItem(at: previewTemporaryDirectory)
            }
        }
        previewTemporaryDirectory = nil
        previewURL = nil
        textFilePreview = nil
        sessionFileShareItem = nil
    }

    func sendMessage() {
        guard connectionState.isReady,
              sessionRequestID == nil,
              let sessionID = selectedSessionID
        else { return }
        let text = composer.trimmingCharacters(in: .whitespacesAndNewlines)
        let attachments = uploadedComposerAttachments
        guard attachments.count <= maximumSessionFileReferences else { return }
        guard !text.isEmpty || !attachments.isEmpty else { return }
        guard attachments.isEmpty || canSubmitAttachments else {
            showToast(attachmentSubmissionUnavailableMessage, tone: .warning)
            return
        }
        guard canSendComposer else { return }
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
        // Past every guard, so a rejected send leaves the keyboard up with the text still
        // there to fix. The send button and the return key both land here, which is why this
        // belongs on the model rather than in the composer's own submit path.
        dismissComposerFocus()
        if pendingWidgetEdit?.recovery.phase == .editing {
            submitComposerEdit(sessionID: sessionID, requestID: id, text: text)
            return
        }
        let stashedText = stashedComposerDraft
        let op: AgentOperation
        if let activeTurnID, let activeOperation {
            op = .activeInput(operation: activeOperation, turnID: activeTurnID, text: text)
        } else {
            op = .userInput(text: text, attachments: attachments)
            startChatTitle(prompt: text, submissionID: id, sessionID: sessionID)
        }
        pendingDrafts[id] = PendingComposerDraft(text: text, attachments: attachments)
        composerDraftSaveTask?.cancel()
        composerDraftSaveTask = nil
        if let owner = composerDraftOwner {
            enqueueComposerDraftSave(stashedText ?? text, owner: owner)
        }
        stashedComposerDraft = nil
        suppressesComposerDraftSave = true
        composer = ""
        suppressesComposerDraftSave = false
        composerAttachments = []
        transmit(.submit(sessionID: sessionID, submission: Submission(id: id, op: op))) { [weak self] _ in
            guard let self else { return }
            self.restoreDraft(id: id)
            self.cancelChatTitle(submissionID: id, rearm: true)
        }
        if let stashedText, !stashedText.isEmpty {
            composer = stashedText
        }
    }

    func editWidgetInputInComposer(_ mounted: MountedWidget) {
        guard connectionState.isReady,
              !isLoadingComposerDraft,
              !isLoadingComposerEditRecovery,
              let sessionID = selectedSessionID,
              let accountID = selectedAccountID,
              let operation = mounted.widget.action,
              let input = operation.capabilityInput
        else { return }
        guard composerAttachments.isEmpty else {
            showToast("Finish the attachment draft before editing a queued message.", tone: .warning)
            return
        }
        guard pendingWidgetEdit == nil, stashedComposerDraft == nil else { return }
        flushComposerDraft()
        let requestID = requestID("edit")
        let owner = ComposerDraftOwner(accountID: accountID, sessionID: sessionID)
        let recovery = ComposerEditRecovery(
            capability: mounted.capability,
            widgetID: mounted.widget.id,
            originalInput: input,
            displacedDraft: composer,
            editedInput: input,
            requestID: requestID,
            submissionBaselineSequence: nil,
            phase: .removingQueuedInput
        )
        pendingWidgetEdit = PendingWidgetEdit(owner: owner, recovery: recovery)
        enqueueComposerEditRecoverySave(recovery, owner: owner) { [weak self] result in
            guard let self,
                  self.pendingWidgetEdit?.owner == owner,
                  self.pendingWidgetEdit?.recovery.requestID == requestID
            else { return }
            if case .failure(let error) = result {
                self.pendingWidgetEdit = nil
                self.showToast(error.localizedDescription, tone: .error)
                return
            }
            guard self.connectionState.isReady, self.selectedSessionID == sessionID else { return }
            guard self.selectedAccountID == accountID else { return }
            self.transmit(.submit(
                sessionID: sessionID,
                submission: Submission(id: requestID, op: operation)
            ))
        }
    }

    private func submitComposerEdit(sessionID: String, requestID: String, text: String) {
        guard var pending = pendingWidgetEdit,
              let accountID = selectedAccountID,
              pending.owner == ComposerDraftOwner(accountID: accountID, sessionID: sessionID),
              pending.recovery.phase == .editing
        else { return }
        let operation: AgentOperation
        if let activeTurnID, let activeOperation {
            operation = .activeInput(operation: activeOperation, turnID: activeTurnID, text: text)
        } else {
            operation = .userInput(text: text, attachments: [])
        }
        pending.recovery.editedInput = text
        pending.recovery.requestID = requestID
        pending.recovery.submissionBaselineSequence = latestSequence
        pending.recovery.phase = .submitting
        pendingWidgetEdit = pending
        composerDraftSaveTask?.cancel()
        composerDraftSaveTask = nil
        enqueueComposerEditRecoverySave(pending.recovery, owner: pending.owner) { [weak self] result in
            guard let self,
                  self.pendingWidgetEdit?.owner == pending.owner,
                  self.pendingWidgetEdit?.recovery.requestID == requestID,
                  self.pendingWidgetEdit?.recovery.phase == .submitting
            else { return }
            if case .failure(let error) = result {
                self.restoreComposerEditMode(requestID: requestID)
                self.showToast(error.localizedDescription, tone: .error)
                return
            }
            guard self.connectionState.isReady, self.selectedSessionID == sessionID else {
                self.restoreComposerEditMode(requestID: requestID)
                return
            }
            guard self.selectedAccountID == pending.owner.accountID else {
                self.restoreComposerEditMode(requestID: requestID)
                return
            }
            self.stashedComposerDraft = nil
            self.suppressesComposerDraftSave = true
            self.composer = pending.recovery.displacedDraft
            self.suppressesComposerDraftSave = false
            self.transmit(
                .submit(
                    sessionID: sessionID,
                    submission: Submission(id: requestID, op: operation)
                )
            ) { [weak self] _ in
                self?.restoreComposerEditMode(requestID: requestID)
            }
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
        if case .capabilityCommand(let capability, _, _, _, _) = operation,
           middlewareFeatures.contains(where: { $0.id == capability }),
           !isCapabilityEnabled(capability) {
            return
        }
        transmit(.submit(
            sessionID: sessionID,
            submission: Submission(id: requestID("widget-action"), op: operation)
        ))
    }

    func loadPreviewPage(_ operation: AgentOperation) {
        guard let sessionID = selectedSessionID, !isLoadingPreviewPage else { return }
        if case .capabilityCommand(let capability, _, _, _, _) = operation,
           middlewareFeatures.contains(where: { $0.id == capability }),
           !isCapabilityEnabled(capability) {
            return
        }
        let id = requestID("preview-page")
        previewPageRequestID = id
        isLoadingPreviewPage = true
        transmit(.submit(
            sessionID: sessionID,
            submission: Submission(id: id, op: operation)
        )) { [weak self] _ in
            guard self?.previewPageRequestID == id else { return }
            self?.previewPageRequestID = nil
            self?.isLoadingPreviewPage = false
        }
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
        modelRoute(for: agentDraft)
    }

    var defaultAgentDraftModelRoute: String? {
        modelRoute(for: defaultAgentDraft)
    }

    private func modelRoute(for draft: AgentComposition?) -> String? {
        guard let provider = draft?.provider else { return nil }
        return modelChoices.first { choice in
            choice.model == provider.model
                && choice.reasoningEffort == provider.reasoningEffort
                && providerStatus(for: choice)?.provider == provider.provider
        }?.route
    }

    func selectAgentDraftModel(_ route: String) {
        agentDraft = draft(agentDraft, selectingModelRoute: route)
    }

    func selectDefaultAgentDraftModel(_ route: String) {
        defaultAgentDraft = draft(defaultAgentDraft, selectingModelRoute: route)
    }

    private func draft(
        _ currentDraft: AgentComposition?,
        selectingModelRoute route: String
    ) -> AgentComposition? {
        guard let choice = modelChoices.first(where: { $0.route == route }),
              let status = providerStatus(for: choice),
              var provider = status.selection,
              var draft = currentDraft
        else { return currentDraft }
        provider.model = choice.model
        provider.reasoningEffort = choice.reasoningEffort
        draft.provider = provider
        return draft
    }

    func modelLabel(for choice: ModelChoice) -> String {
        modelLabel(provider: modelProviders[choice.route], modelID: choice.model)
    }

    func modelLabel(provider: String?, modelID: String) -> String {
        guard let provider else { return modelID }
        return providerStatuses
            .first { $0.provider == provider }?
            .models.first { $0.id == modelID }?
            .label ?? modelID
    }

    func providerLabel(for provider: String) -> String {
        providerStatuses.first { $0.provider == provider }?.label ?? provider
    }

    func providerLabel(for choice: ModelChoice) -> String {
        guard let provider = modelProviders[choice.route] else { return choice.group }
        return providerLabel(for: provider)
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

    func showFiles(_ tab: FilesInspectorTab? = nil) {
        if let tab { filesInspectorTab = tab }
        showsInspector = true
        refreshFiles(for: filesInspectorTab)
    }

    func toggleFilesInspector() {
        if showsInspector {
            showsInspector = false
        } else {
            showFiles()
        }
    }

    private func refreshFiles(for tab: FilesInspectorTab) {
        switch tab {
        case .unstaged: refreshGitDiff()
        case .allFiles: refreshWorkspaceFiles()
        case .chatFiles: refreshChatFiles()
        }
    }

    func changeAgentForCurrentChat() {
        applyAgentConfiguration(agentDraft, to: .session)
    }

    func saveAgentAsDefault() {
        applyAgentConfiguration(defaultAgentDraft, to: .defaultAgent)
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

    private func applyAgentConfiguration(
        _ draft: AgentComposition?,
        to target: ConfigurationTarget
    ) {
        guard !isApplyingConfiguration, let draft else { return }
        let id = requestID("configure")
        switch target {
        case .session:
            guard let sessionID = selectedSessionID, let snapshot = agentSnapshot else {
                chatAgentApplyState = .idle
                return
            }
            chatAgentApplyState = .applying
            configRequestID = id
            transmit(.configureSession(
                requestID: id,
                sessionID: sessionID,
                expectedRevision: snapshot.revision,
                config: draft
            )) { [weak self] message in
                guard self?.configRequestID == id else { return }
                self?.configRequestID = nil
                self?.chatAgentApplyState = .failed(message)
            }
        case .defaultAgent:
            guard let snapshot = defaultAgentSnapshot else {
                defaultAgentApplyState = .failed(
                    "The gateway has no default agent configuration."
                )
                return
            }
            defaultAgentApplyState = .applying
            defaultConfigRequestID = id
            submittedDefaultAgentDraft = draft
            transmit(.configureDefaultAgent(
                requestID: id,
                expectedRevision: snapshot.revision,
                config: draft
            )) { [weak self] message in
                guard self?.defaultConfigRequestID == id else { return }
                self?.defaultConfigRequestID = nil
                self?.submittedDefaultAgentDraft = nil
                self?.defaultAgentApplyState = .failed(message)
            }
        }
    }

    func reloadAgentDraft() {
        agentDraft = agentSnapshot?.config
        chatAgentApplyState = .idle
        showToast("Agent draft reloaded.", tone: .info)
    }

    func reloadDefaultAgentDraft() {
        defaultAgentDraft = defaultAgentSnapshot?.config
        defaultAgentApplyState = .idle
        showToast("Default agent draft reloaded.", tone: .info)
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
        providerReasoningEffortsText = status.reasoningEfforts.joined(separator: ", ")
        providerAPIKey = ""
        providerActionState = .idle
    }

    var providerModelIDs: [String] {
        commaSeparatedValues(providerModelIDsText)
    }

    var providerReasoningEfforts: [String] {
        commaSeparatedValues(providerReasoningEffortsText)
    }

    private func commaSeparatedValues(_ text: String) -> [String] {
        text
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
        providerDraft?.reasoningEffort = providerReasoningEfforts.first
    }

    func updateProviderReasoningEfforts(_ value: String) {
        providerReasoningEffortsText = value
        providerDraft?.reasoningEffort = providerReasoningEfforts.first
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

    func saveProviderAsDefault() {
        registerProvider()
    }

    private func registerProvider() {
        guard var config = defaultAgentDraft?.provider ?? setupProviderDraft,
              let status = providerStatuses.first(where: { $0.provider == config.provider })
        else { return }
        let modelIDs = status.modelIdsConfigurable ? providerModelIDs : status.modelIds
        let reasoningEfforts = status.modelIdsConfigurable
            ? providerReasoningEfforts
            : status.reasoningEfforts
        if status.modelIdsConfigurable {
            guard let first = modelIDs.first else { return }
            config.model = first
            config.reasoningEffort = reasoningEfforts.first
        }
        defaultAgentDraft?.provider = config
        let id = requestID("provider")
        providerRegistrationRequestID = id
        defaultAgentApplyState = .applying
        transmit(.registerProvider(
            requestID: id,
            config: config,
            modelIds: modelIDs,
            reasoningEfforts: reasoningEfforts
        )) { [weak self] message in
            guard self?.providerRegistrationRequestID == id else { return }
            self?.providerRegistrationRequestID = nil
            self?.defaultAgentApplyState = .failed(message)
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
        guard canStartCronSetup, let sessionID = selectedSessionID else { return }
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
        guard isSchedulingEnabled, let sessionID = selectedSessionID else { return }
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
        guard isSchedulingEnabled, let sessionID = selectedSessionID else { return }
        let request = requestID("cron-delete")
        cronRequestIDs.insert(request)
        transmit(.deleteCron(requestID: request, sessionID: sessionID, id: task.id)) { [weak self] message in
            self?.cronRequestIDs.remove(request)
            self?.cronError = message
        }
    }

    func runCron(_ task: CronTask) {
        guard isSchedulingEnabled, let sessionID = selectedSessionID else { return }
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
        settingsDefaults.set(theme.rawValue, forKey: "theme")
    }

    func setSharesHorusDiagnostics(_ sharesDiagnostics: Bool) {
        sharesHorusDiagnostics = sharesDiagnostics
        settingsDefaults.set(sharesDiagnostics, forKey: sharesHorusDiagnosticsKey)
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
        cancelReconnect()
        reconnectsOnActivation = true
        flushComposerDraft()
        guard appLockEnabled else { return }
        discardFilePresentation()
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

    private func connect(to account: GatewayAccount, retrying: Bool = false) {
        cancelReconnect()
        if !retrying {
            reconnectAttempt = 0
            automaticReconnectBlocked = false
        }
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
                    try await self.requestSender(.authenticate(
                        token: token,
                        clientKind: .currentApplePlatform
                    ))
                }
            } catch {
                self.automaticReconnectBlocked = true
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
                let stream = try await self.connectionOpener(endpoint)
                guard generation == self.connectionGeneration else { return }
                self.connectionState = .authenticating
                self.eventTask = Task { [weak self] in
                    do {
                        var handledFrames = 0
                        for try await frame in stream {
                            guard let self, generation == self.connectionGeneration else { return }
                            self.handle(frame)
                            handledFrames += 1
                            if handledFrames.isMultiple(of: 32) { await Task.yield() }
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
        case .sessionHistory(
            let requestID,
            let sessionID,
            let records,
            let nextBeforeSequence
        ):
            guard requestID == historyRequestID, sessionID == selectedSessionID else { break }
            historyRequestID = nil
            isLoadingEarlierHistory = false
            mergeHistory(records)
            self.nextHistoryBeforeSequence = nextBeforeSequence
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
        case .agentEvent(let sessionID, let record):
            guard sessionID == selectedSessionID else { break }
            let buffered = BufferedAgentEvent(record: record)
            applyAgentEvent(buffered)
            if replayRequestID == nil, shouldCacheTranscript(after: record.event) {
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
        case .artifacts(let requestID, let sessionID, let artifacts, let truncated):
            guard requestID == artifactListRequestID, sessionID == selectedSessionID else { break }
            artifactListRequestID = nil
            isLoadingArtifacts = false
            self.artifacts = artifacts
            artifactsTruncated = truncated
        case .gitDiff(let requestID, let sessionID, let scope, let diff):
            guard requestID == gitDiffRequestID,
                  sessionID == selectedSessionID,
                  scope == .unstaged
            else { break }
            gitDiffRequestID = nil
            isLoadingGitDiff = false
            gitDiff = diff
        case .workspaceFiles(let requestID, let sessionID, let files):
            guard requestID == workspaceFilesRequestID,
                  sessionID == selectedSessionID
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
        case .sessionFileUploadReady(let requestID, let sessionID, let uploadID, let maxChunkBytes):
            handleSessionFileUploadReady(
                requestID: requestID,
                sessionID: sessionID,
                uploadID: uploadID,
                maxChunkBytes: maxChunkBytes
            )
        case .sessionFileUploadChunkAccepted(let requestID, let sessionID, let uploadID, let nextOffset):
            handleSessionFileUploadChunkAccepted(
                requestID: requestID,
                sessionID: sessionID,
                uploadID: uploadID,
                nextOffset: nextOffset
            )
        case .sessionFileUploadCompleted(let requestID, let sessionID, let file):
            handleSessionFileUploadCompleted(
                requestID: requestID,
                sessionID: sessionID,
                file: file
            )
        case .sessionUploads(let requestID, let sessionID, let uploads):
            guard requestID == sessionUploadsRequestID, sessionID == selectedSessionID else { break }
            sessionUploadsRequestID = nil
            isLoadingSessionUploads = false
            sessionUploads = uploads
        case .sessionFileChunk(
            let requestID,
            let sessionID,
            let fileID,
            let offset,
            let data,
            let nextOffset
        ):
            handleSessionFileChunk(
                requestID: requestID,
                sessionID: sessionID,
                fileID: fileID,
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
                automaticReconnectBlocked = true
                cancelReconnect()
                repairSelectedGateway()
            }
            showToast(failure.message, tone: .error)
            if failure.fatal {
                automaticReconnectBlocked = true
                cancelReconnect()
                connectionGeneration = UUID()
                eventTask?.cancel()
                eventTask = nil
                restorePendingDrafts()
                connectionState = .failed(failure.message)
            }
        }
    }

    private func applyAgentEvent(_ buffered: BufferedAgentEvent) {
        guard latestSequence.map({ buffered.record.sequence > $0 }) ?? true else { return }
        observeReplayCompletion(buffered)
        latestSequence = buffered.record.sequence
        transcriptRecords[buffered.record.sequence] = buffered.record
        reduce(
            record: buffered.record
        )
    }

    private func finishSessionReplay() {
        flushStreamDeltas()
        if let replaySnapshotSequence { latestSequence = replaySnapshotSequence }
        replayRequestID = nil
        replaySnapshotSequence = nil
        replayPresentedTranscript = nil
        connectionState = .ready
        completedComposerEditReplay = true
        reconcileChatTitleAfterReplay()
        reconcileComposerEditRecovery()
        requestSessionData()
        cacheSelectedTranscript()
    }

    /// A disconnected submission is ambiguous until replay proves whether it reached the
    /// checkpoint. Only then can the restored draft safely become title-eligible again.
    private func reconcileChatTitleAfterReplay() {
        guard let sessionID = selectedSessionID,
              let pending = pendingChatTitles[sessionID],
              !pending.submissionConfirmed
        else { return }
        let promptWasReplayed = replayCompletionSubmissionIDs.contains(
            pending.attempt.submissionID
        ) || replayUserMessages.contains {
            $0.text.trimmingCharacters(in: .whitespacesAndNewlines) == pending.attempt.prompt
        }
        if promptWasReplayed {
            confirmChatTitle(sessionID: sessionID)
        } else {
            cancelChatTitle(sessionID, rearm: true)
        }
    }

    private func shouldCacheTranscript(after event: AgentEventRecord) -> Bool {
        switch event.msg["type"]?.stringValue {
        case "task_complete", "turn_aborted": true
        default: false
        }
    }

    private func cacheSelectedTranscript() {
        guard let accountID = selectedAccountID,
              let sessionID = selectedSessionID,
              let latestSequence,
              activeTurnID == nil,
              pendingApproval == nil,
              pendingWidgetEdit == nil
        else { return }
        let snapshot = CachedTranscript(
            sequence: latestSequence,
            nextBeforeSequence: nextHistoryBeforeSequence,
            transcript: transcript,
            currentUsage: currentUsage,
            lastUsage: lastUsage
        )
        enqueueTranscriptIO { [store] in
            await store.saveTranscript(
                snapshot,
                accountID: accountID,
                sessionID: sessionID
            )
        }
    }

    private func applyGatewayReady(_ payload: ReadyPayload) {
        cancelReconnect()
        reconnectAttempt = 0
        automaticReconnectBlocked = false
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
        let registeredProviderDraft = requestID == providerRegistrationRequestID
            ? defaultAgentDraft
            : nil
        let editedDefaultDraft = requestID == defaultConfigRequestID
            ? defaultAgentDraft
            : nil
        applyGatewayReady(payload)
        if requestID == providerRegistrationRequestID {
            providerRegistrationRequestID = nil
            defaultAgentApplyState = .idle
            if let registeredProviderDraft { defaultAgentDraft = registeredProviderDraft }
            applyAgentConfiguration(defaultAgentDraft, to: .defaultAgent)
        } else if requestID == defaultConfigRequestID {
            defaultConfigRequestID = nil
            if let editedDefaultDraft,
               let submittedDefaultAgentDraft,
               editedDefaultDraft != submittedDefaultAgentDraft {
                defaultAgentDraft = editedDefaultDraft
            }
            submittedDefaultAgentDraft = nil
            defaultAgentApplyState = .applied
            showToast("Default agent saved for new chats.", tone: .success)
        }
    }

    func applyGatewayCatalog(_ payload: ReadyPayload) {
        gatewayMachineName = payload.machineName
        let previousDefault = defaultAgentSnapshot
        let pendingDefaultDraft: AgentComposition? = if defaultConfigRequestID != nil
            || providerRegistrationRequestID != nil {
            defaultAgentDraft
        } else {
            nil
        }
        providerStatuses = payload.providers
        modelChoices = payload.models
        modelProviders = payload.modelProviders
        middlewareFeatures = payload.middlewareFeatures
        defaultAgentSnapshot = payload.defaultConfig
        defaultAgentDraft = payload.defaultConfig.map { incomingSnapshot in
            pendingDefaultDraft ?? refreshedAgentDraft(
                currentDraft: defaultAgentDraft,
                currentSnapshot: previousDefault,
                incomingSnapshot: incomingSnapshot
            )
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
        let createdByThisClient = opened && isChangingWorkspace
        let cursor = sessionOpenCursor
        let cached = opened && sessionOpeningID == payload.session.sessionId
            ? pendingCachedTranscript
            : nil
        let presented = opened && sessionOpeningID == payload.session.sessionId
            ? pendingPresentedTranscript
            : nil
        if selectedSessionID != payload.session.sessionId {
            restorePendingDrafts()
            changeComposerDraftOwner(to: selectedAccountID.map {
                ComposerDraftOwner(accountID: $0, sessionID: payload.session.sessionId)
            })
            resetSessionState()
        }
        if opened {
            latestSequence = cursor
            self.replayRequestID = replayRequestID
            replaySnapshotSequence = payload.latestSequence
            sessionOpenCursor = nil
            sessionOpeningID = nil
            pendingCachedTranscript = nil
            pendingPresentedTranscript = nil
            replayPresentedTranscript = presented ?? []
            transcriptRecordBase = cached?.transcript ?? []
            transcriptRecordBaseSequence = cursor
            transcriptRecords.removeAll(keepingCapacity: true)
            transcript = cached?.transcript ?? []
            if let cached {
                nextHistoryBeforeSequence = cached.nextBeforeSequence
            } else {
                nextHistoryBeforeSequence = payload.nextBeforeSequence
            }
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
        if createdByThisClient { prepareChatTitle(for: payload.session.sessionId) }
        if isChatVisible {
            unreadSessionIDs.remove(payload.session.sessionId)
        }
        selectedModelRoute = payload.session.model.route
        modelContextWindow = payload.session.model.modelContextWindow
        contributions = payload.contributions
        mountedWidgets = payload.contributions.flatMap { contribution in
            contribution.widgets.map {
                MountedWidget(capability: contribution.capability, widget: $0)
            }
        }
        for widget in payload.widgets {
            upsertWidget(MountedWidget(capability: widget.capability, widget: widget.item))
        }
        runStats = payload.runStats
        activeTurnID = payload.runStats.active?.turnId
        awaitsSteeringDelivery = false
        activeOperation = payload.contributions.compactMap(\.activeInput?.operation).first
        agentDraft = refreshedAgentDraft(
            currentDraft: agentDraft,
            currentSnapshot: agentSnapshot,
            incomingSnapshot: payload.config
        )
        agentSnapshot = payload.config
        if !opened { connectionState = .ready }
        if let accountID = selectedAccountID {
            prepareComposerEditRecovery(
                for: ComposerDraftOwner(
                    accountID: accountID,
                    sessionID: payload.session.sessionId
                )
            )
        }
        if chatAgentApplyState == .restarting {
            chatAgentApplyState = .applied
            showToast("Agent configuration applied.", tone: .success)
        }
        persistGeneratedChatTitles()
    }

    func applySessions(_ records: [SessionRecord]) {
        let visibleSessions = records.filter(\.catalogVisible)
        guard Set(visibleSessions.map(\.sessionId)).count == visibleSessions.count else {
            showToast("The gateway returned duplicate chat identifiers.", tone: .error)
            return
        }
        if sessions != visibleSessions {
            let previous = Dictionary(
                sessions.map { ($0.sessionId, $0.activity) },
                uniquingKeysWith: { _, latest in latest }
            )
            sessions = visibleSessions
            for session in sessions {
                applyActivityTransition(
                    from: previous[session.sessionId],
                    to: session.activity,
                    sessionID: session.sessionId
                )
            }
        }
        if let selected = sessions.first(where: { $0.sessionId == selectedSessionID }) {
            applyExecutionStats(selected.executionStats)
            if selected.activity.state == .idle { runStats.active = nil }
        }
        let visible = Set(sessions.map(\.sessionId))
        unreadSessionIDs.formIntersection(visible)
        reconcileChatTitles()
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
        refreshWorkspaceChanges()
        refreshChatFiles()
        refreshCron()
    }

    private func clearSelectedSession() {
        changeComposerDraftOwner(to: nil)
        latestSequence = nil
        sessionOpenCursor = nil
        selectedSessionID = nil
        resetSessionState()
        connectionState = .ready
    }

    private func handleAccepted(_ requestID: String) {
        if pendingDrafts[requestID] != nil { flushComposerDraft() }
        if requestID == approvalRequestID {
            pendingApproval = nil
            approvalRequestID = nil
        }
        if requestID == configRequestID {
            chatAgentApplyState = .restarting
            configRequestID = nil
        }
        if requestID == sessionMutationRequestID {
            if let sessionID = pendingDeletedSessionID {
                cancelChatTitle(sessionID)
                if let accountID = selectedAccountID {
                    let owner = ComposerDraftOwner(accountID: accountID, sessionID: sessionID)
                    invalidateComposerEditRecovery(for: owner)
                    enqueueComposerDraftSave("", owner: owner)
                    enqueueComposerEditRecoveryRemoval(owner: owner)
                    if composerDraftOwner == owner { discardComposerDraft() }
                }
            }
            pendingDeletedSessionID = nil
            transmit(.listSessions(requestID: requestID)) { [weak self] _ in
                if self?.sessionMutationRequestID == requestID {
                    self?.sessionMutationRequestID = nil
                }
            }
        }
        if requestID == gitBranchRequestID {
            gitBranchRequestID = nil
            showToast("Git branch changed.", tone: .success)
            refreshWorkspaceChanges()
        }
        if cronRequestIDs.remove(requestID) != nil {
            cronTaskDraft = ""
            refreshCron()
        }
    }

    private func handleRejected(_ rejection: GatewayRejection) {
        if rejection.requestId == historyRequestID {
            historyRequestID = nil
            isLoadingEarlierHistory = false
        }
        if rejection.requestId == previewPageRequestID {
            previewPageRequestID = nil
            isLoadingPreviewPage = false
        }
        if rejection.requestId == sessionMutationRequestID {
            pendingDeletedSessionID = nil
            if let sessionID = pendingChatTitles.first(where: {
                $0.value.renameRequestID == rejection.requestId
            })?.key {
                cancelChatTitle(sessionID)
            }
        }
        cancelChatTitle(submissionID: rejection.requestId, rearm: true)
        if rejection.requestId == sessionRequestID,
           rejection.code == "replay_unavailable",
           let sessionID = sessionOpeningID,
           sessionOpenCursor != nil {
            if let accountID = selectedAccountID {
                enqueueTranscriptIO { [store] in
                    await store.removeTranscript(accountID: accountID, sessionID: sessionID)
                }
            }
            sessionRequestID = nil
            sessionOpenCursor = nil
            pendingCachedTranscript = nil
            pendingPresentedTranscript = nil
            if sessionID == selectedSessionID { resetSessionState() }
            requestSessionOpen(sessionID, lastSequence: nil)
            return
        }
        failSessionFileUploadRequest(rejection.requestId, message: rejection.message, showsToast: false)
        if rejection.requestId == sessionUploadsRequestID {
            sessionUploadsRequestID = nil
            isLoadingSessionUploads = false
        }
        if rejection.requestId == artifactListRequestID {
            artifactListRequestID = nil
            isLoadingArtifacts = false
        }
        if rejection.requestId == sessionFileDownload?.requestID {
            sessionFileDownload = nil
            isLoadingFilePresentation = false
        }
        if rejection.requestId == workspaceFilePreviewDownload?.requestID {
            workspaceFilePreviewDownload = nil
            isLoadingFilePresentation = false
        }
        if pendingDrafts[rejection.requestId] != nil {
            restoreDraft(id: rejection.requestId)
        }
        rejectComposerEdit(requestID: rejection.requestId)
        if rejection.requestId == configRequestID
            || rejection.requestId == defaultConfigRequestID {
            let state: ApplyState = switch rejection.code {
            case "revision_conflict": .conflict(rejection.message)
            case "agent_busy": .busy(rejection.message)
            case "invalid_config": .invalid(rejection.message)
            default: .failed(rejection.message)
            }
            if rejection.requestId == configRequestID {
                chatAgentApplyState = state
                configRequestID = nil
            }
            if rejection.requestId == defaultConfigRequestID {
                defaultAgentApplyState = state
                defaultConfigRequestID = nil
                submittedDefaultAgentDraft = nil
            }
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
            defaultAgentApplyState = .failed(rejection.message)
            providerRegistrationRequestID = nil
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
            automaticReconnectBlocked = true
            cancelReconnect()
            connectionGeneration = UUID()
            eventTask?.cancel()
            eventTask = nil
            restorePendingDrafts()
            connectionState = .failed(rejection.message)
        }
    }

    func reduce(record: RecordedEvent) {
        let event = record.event
        let blocks = record.blocks
        let type = event.msg["type"]?.stringValue ?? "unknown"
        // Queue removal also happens for edits and turn cleanup; only the immediately
        // following targeted user message proves that steering reached the model input.
        let confirmsSteeringDelivery = awaitsSteeringDelivery
            && replayRequestID == nil
            && type == "user_message"
            && event.submissionId != nil
            && messageTarget(from: event.msg) != nil
        awaitsSteeringDelivery = false
        if confirmsSteeringDelivery { steeringDeliveryRevision &+= 1 }
        // Anything that is not a delta may read or finalize the streams the buffer feeds,
        // so buffered text must land first to keep transcript order exact.
        if type != "agent_message_content_delta", type != "agent_reasoning_content_delta" {
            flushStreamDeltas()
        }
        let wasRendered = !blocks.isEmpty
        if type == "user_message", let submissionID = event.submissionId {
            confirmChatTitle(submissionID: submissionID)
        }
        if let submissionID = event.submissionId {
            if type == "warning" || type == "error" {
                if let draft = pendingDrafts.removeValue(forKey: submissionID) { restoreDraft(draft) }
                previewSelections.removeValue(forKey: submissionID)
                if previewPageRequestID == submissionID {
                    previewPageRequestID = nil
                    isLoadingPreviewPage = false
                }
                rejectComposerEdit(requestID: submissionID)
            } else {
                pendingDrafts.removeValue(forKey: submissionID)
                if type == "user_message"
                    || (type == "frontend"
                        && event.msg["frontendType"]?.stringValue == "widget") {
                    completeSubmittedComposerEdit(requestID: submissionID)
                }
                flushComposerDraft()
            }
        }

        for (index, rendered) in blocks.enumerated() {
            apply(
                rendered,
                sequence: record.sequence,
                blockIndex: index,
                recordedAtMs: record.recordedAtMs
            )
        }
        if let preview = record.preview {
            if event.submissionId == previewPageRequestID {
                previewPageRequestID = nil
                isLoadingPreviewPage = false
            }
            apply(
                preview,
                selection: event.submissionId.flatMap { previewSelections.removeValue(forKey: $0) }
            )
        }

        switch type {
        case "user_message":
            let attachments = event.msg["attachments"]?.arrayValue?.compactMap {
                try? SessionFileReference(json: $0)
            } ?? []
            appendText(
                event.msg["message"]?.stringValue,
                kind: .user,
                id: "event:\(record.sequence):user",
                sourceSequence: record.sequence,
                recordedAtMs: record.recordedAtMs,
                messageTarget: messageTarget(from: event.msg),
                files: attachments
            )
        case "agent_message_content_delta":
            let phase = event.msg["phase"]?.stringValue
            guard let modelStepID = event.msg["modelStepId"]?.stringValue else { return }
            let commentary = phase == "commentary"
            appendStream(
                id: streamID(modelStepID: modelStepID, phase: phase ?? "final_answer"),
                delta: event.msg["delta"]?.stringValue ?? "",
                kind: commentary ? .commentary : .assistant,
                modelStepID: modelStepID,
                record: record
            )
        case "agent_reasoning_content_delta":
            guard let modelStepID = event.msg["modelStepId"]?.stringValue else { return }
            appendStream(
                id: streamID(modelStepID: modelStepID, phase: "reasoning"),
                delta: event.msg["delta"]?.stringValue ?? "",
                kind: .reasoning,
                modelStepID: modelStepID,
                record: record
            )
        case "model_step_completed":
            applyModelStepCompletion(event.msg, record: record)
        case "model_step_started":
            if replayRequestID == nil { runStats.active?.modelCalls += 1 }
        case "agent_message":
            let phase = event.msg["phase"]?.stringValue
            let kind: TranscriptEntry.Kind = phase == "commentary" ? .commentary : .assistant
            if let modelStepID = event.msg["modelStepId"]?.stringValue,
               transcript.contains(where: {
                   $0.modelStepID == modelStepID && $0.kind == kind && !$0.pending
               }) {
                if let messageTarget = messageTarget(from: event.msg),
                   let index = transcript.lastIndex(where: {
                       $0.modelStepID == modelStepID && $0.kind == kind
                   }) {
                    transcript[index].messageTarget = messageTarget
                }
            } else if wasRendered {
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
            refreshWorkspaceChanges()
            pendingApproval = nil
            approvalRequestID = nil
        case "web_search_begin":
            break
        case "web_search_end":
            break
        case "turn_aborted":
            finishPendingTranscriptEntries()
            activeTurnID = nil
            if replayRequestID == nil { runStats.active = nil }
            refreshWorkspaceChanges()
            pendingApproval = nil
            approvalRequestID = nil
            if !wasRendered { finishPendingTranscriptEntries() }
        case "warning":
            break
        case "error":
            break
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
            applyFrontendEvent(event.msg, submissionID: event.submissionId)
        default:
            break
        }
    }

    private func applyFrontendEvent(_ event: JSONValue, submissionID: String?) {
        switch event["frontendType"]?.stringValue {
        case "render":
            break
        case "widget":
            guard let capability = event["capability"]?.stringValue,
                  let item = event["item"],
                  let widget = try? FrontendWidget(json: item)
            else { return }
            upsertWidget(MountedWidget(capability: capability, widget: widget))
            acknowledgeWidgetEdit(
                submissionID: submissionID,
                capability: capability,
                widgetID: widget.id
            )
        case "remove_widget":
            guard let capability = event["capability"]?.stringValue,
                  let id = event["id"]?.stringValue
            else { return }
            mountedWidgets.removeAll { $0.capability == capability && $0.widget.id == id }
            if replayRequestID == nil,
               contributions.contains(where: {
                   $0.capability == capability && $0.activeInput != nil
               }) {
                awaitsSteeringDelivery = true
            }
            acknowledgeWidgetEdit(
                submissionID: submissionID,
                capability: capability,
                widgetID: id
            )
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

    private func acknowledgeWidgetEdit(
        submissionID: String?,
        capability: String,
        widgetID: String
    ) {
        guard var pending = pendingWidgetEdit,
              pending.recovery.phase == .removingQueuedInput,
              pending.recovery.requestID == submissionID,
              pending.recovery.capability == capability,
              pending.recovery.widgetID == widgetID
        else { return }
        pending.recovery.phase = .editing
        pendingWidgetEdit = pending
        flushComposerDraft()
        stashedComposerDraft = pending.recovery.displacedDraft
        suppressesComposerDraftSave = true
        composer = pending.recovery.editedInput
        suppressesComposerDraftSave = false
        composerFocusRequest &+= 1
        enqueueComposerEditRecoverySave(pending.recovery, owner: pending.owner)
    }

    private func upsertWidget(_ mounted: MountedWidget) {
        if let index = mountedWidgets.firstIndex(where: { $0.id == mounted.id }) {
            mountedWidgets[index] = mounted
        } else {
            mountedWidgets.append(mounted)
        }
    }

    private func apply(
        _ rendered: RenderedBlock,
        sequence: UInt64,
        blockIndex: Int,
        recordedAtMs: Int64
    ) {
        apply(
            rendered,
            sequence: sequence,
            blockIndex: blockIndex,
            recordedAtMs: recordedAtMs,
            to: &transcript
        )
        let block = rendered.block
        if block.format == "unified_diff", !block.pending {
            refreshWorkspaceChanges()
            refreshArtifacts()
        } else if !block.files.isEmpty, !block.pending {
            refreshArtifacts()
        }
    }

    private func apply(
        _ rendered: RenderedBlock,
        sequence: UInt64,
        blockIndex: Int,
        recordedAtMs: Int64,
        recordID: String? = nil,
        to entries: inout [TranscriptEntry]
    ) {
        let block = rendered.block
        let sourceID = block.id
            ?? recordID.map { "record:\($0):\(blockIndex)" }
            ?? "record:\(sequence):\(blockIndex)"
        let id = scopedBlockID(capability: rendered.capability, sourceID: sourceID)
        let appending = block.update == .append
        let kind: TranscriptEntry.Kind = block.tone == "error" ? .error : .event
        if let index = entries.firstIndex(where: { $0.id == id }) {
            let previousUpdate = entries[index].update
            // Grouping keys off kind and role, and both can change on an entry that is
            // already on screen — an event turning into an error keeps its id. The row
            // cache cannot see that from the array alone, so it is told here.
            if entries[index].kind != kind || entries[index].role != block.role {
                invalidateTranscriptRows()
            }
            entries[index].text = appending ? entries[index].text + block.text : block.text
            entries[index].kind = kind
            entries[index].capability = rendered.capability
            entries[index].role = block.role
            entries[index].update = appending && previousUpdate == .append ? .append : .replace
            entries[index].title = block.title
            entries[index].symbol = block.symbol
            if block.group != nil { entries[index].group = block.group }
            entries[index].pending = block.pending
            entries[index].sourceSequence = sequence
            entries[index].recordedAtMs = recordedAtMs
            entries[index].format = block.format
            entries[index].tone = block.tone
            let currentFiles = entries[index].files
            entries[index].files = mergedFiles(
                currentFiles,
                with: block.files,
                appending: appending
            )
        } else {
            entries.append(TranscriptEntry(
                id: id,
                text: appending && recordID == nil
                    ? String(block.text.drop(while: { $0 == "\n" }))
                    : block.text,
                kind: kind,
                capability: rendered.capability,
                role: block.role,
                update: block.update,
                title: block.title,
                symbol: block.symbol,
                group: block.group,
                format: block.format,
                tone: block.tone,
                pending: block.pending,
                sourceSequence: sequence,
                recordedAtMs: recordedAtMs,
                files: block.files
            ))
        }
    }

    private func scopedBlockID(capability: String, sourceID: String) -> String {
        "block:\(capability.utf8.count):\(capability)\(sourceID)"
    }

    private func mergedFiles(
        _ current: [SessionFileReference],
        with incoming: [SessionFileReference],
        appending: Bool
    ) -> [SessionFileReference] {
        guard appending else { return incoming }
        var result = current
        for file in incoming {
            if let index = result.firstIndex(where: { $0.id == file.id }) {
                result[index] = file
            } else {
                result.append(file)
            }
        }
        return result
    }

    private func apply(_ preview: RenderedPreview, selection: FrontendPickerOption?) {
        var pageEntries: [TranscriptEntry] = []
        for (index, rendered) in preview.events.enumerated() {
            reduceHistory(
                RecordedEvent(
                    sequence: UInt64(index + 1),
                    recordedAtMs: 0,
                    event: AgentEventRecord(submissionId: nil, msg: rendered.event),
                    streamMetrics: [],
                    blocks: rendered.blocks,
                    preview: nil
                ),
                into: &pageEntries,
                recordID: "\(preview.pageId):\(index)"
            )
        }
        let existing = previews.first { $0.id == preview.id }
        let visibleEntries = switch preview.update {
        case .replace:
            pageEntries
        case .prepend:
            mergePreviewPages(older: pageEntries, newer: existing?.entries ?? [])
        }
        let record = TranscriptPreview(
            id: preview.id,
            title: preview.title,
            context: preview.subtitle.isEmpty ? existing?.context ?? "" : preview.subtitle,
            status: selection?.description ?? existing?.status,
            model: selection?.detail ?? existing?.model,
            entries: visibleEntries,
            next: preview.next
        )
        if let index = previews.firstIndex(where: { $0.id == preview.id }) {
            previews[index] = record
        } else {
            previews.append(record)
        }
        if selection != nil || presentedPreview?.id == preview.id { presentedPreview = record }
    }

    private func mergePreviewPages(
        older: [TranscriptEntry],
        newer: [TranscriptEntry]
    ) -> [TranscriptEntry] {
        var merged = copiedTranscript(older)
        var indices: [String: Int] = [:]
        for index in merged.indices { indices[merged[index].id] = index }
        for source in newer {
            let entry = copiedTranscript([source])[0]
            if let index = indices[entry.id] {
                let previous = merged[index]
                if entry.update == .append {
                    entry.text = previous.text + entry.text
                    entry.files = mergedFiles(previous.files, with: entry.files, appending: true)
                    if entry.group == nil { entry.group = previous.group }
                    entry.update = previous.update == .append ? .append : .replace
                } else if entry.modelStepID != nil, entry.pending, previous.pending {
                    entry.text = previous.text + entry.text
                }
                merged[index] = entry
            } else {
                indices[entry.id] = merged.count
                merged.append(entry)
            }
        }
        return merged
    }

    private func appendText(
        _ text: String?,
        kind: TranscriptEntry.Kind,
        tone: String = "neutral",
        id: String? = nil,
        sourceSequence: UInt64? = nil,
        recordedAtMs: Int64? = nil,
        messageTarget: MessageTarget? = nil,
        files: [SessionFileReference] = []
    ) {
        appendText(
            text,
            kind: kind,
            tone: tone,
            id: id,
            sourceSequence: sourceSequence,
            recordedAtMs: recordedAtMs,
            messageTarget: messageTarget,
            files: files,
            to: &transcript
        )
    }

    private func appendText(
        _ text: String?,
        kind: TranscriptEntry.Kind,
        tone: String = "neutral",
        id: String? = nil,
        sourceSequence: UInt64? = nil,
        recordedAtMs: Int64? = nil,
        messageTarget: MessageTarget? = nil,
        files: [SessionFileReference] = [],
        to entries: inout [TranscriptEntry]
    ) {
        let text = text ?? ""
        guard !text.isEmpty || !files.isEmpty else { return }
        entries.append(TranscriptEntry(
            id: id ?? UUID().uuidString,
            text: text,
            kind: kind,
            format: "plain_text",
            tone: tone,
            pending: false,
            sourceSequence: sourceSequence,
            recordedAtMs: recordedAtMs,
            messageTarget: messageTarget,
            files: files
        ))
    }

    // Deltas arrive several times per frame, and every application re-lays-out the whole
    // growing message. Batching to ~20 flushes a second keeps the text pipeline off the
    // critical path; ordering against non-delta events is preserved by the flush in `reduce`.
    private func streamID(modelStepID: String, phase: String) -> String {
        "model-stream:\(modelStepID.utf8.count):\(modelStepID)\(phase)"
    }

    private func snapshotID(
        modelStepID: String,
        phase: String,
        outputIndex: Int,
        partIndex: Int
    ) -> String {
        "model-output:\(modelStepID.utf8.count):\(modelStepID):\(phase):\(outputIndex):\(partIndex)"
    }

    private func appendStream(
        id: String,
        delta: String,
        kind: TranscriptEntry.Kind,
        modelStepID: String,
        record: RecordedEvent
    ) {
        guard !delta.isEmpty else { return }
        if let last = bufferedDeltas.indices.last, bufferedDeltas[last].id == id {
            bufferedDeltas[last].delta += delta
            bufferedDeltas[last].sourceSequence = record.sequence
            bufferedDeltas[last].recordedAtMs = record.recordedAtMs
        } else {
            bufferedDeltas.append((
                id: id,
                delta: delta,
                kind: kind,
                modelStepID: modelStepID,
                sourceSequence: record.sequence,
                recordedAtMs: record.recordedAtMs
            ))
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
                transcript[index].sourceSequence = buffered.sourceSequence
                transcript[index].recordedAtMs = buffered.recordedAtMs
            } else {
                transcript.append(TranscriptEntry(
                    id: buffered.id,
                    text: buffered.delta,
                    kind: buffered.kind,
                    format: "plain_text",
                    tone: "neutral",
                    pending: true,
                    modelStepID: buffered.modelStepID,
                    sourceSequence: buffered.sourceSequence,
                    recordedAtMs: buffered.recordedAtMs
                ))
            }
        }
        bufferedDeltas.removeAll()
    }

    private func applyModelStepCompletion(_ event: JSONValue, record: RecordedEvent) {
        applyModelStepCompletion(event, record: record, to: &transcript)
    }

    private func applyModelStepCompletion(
        _ event: JSONValue,
        record: RecordedEvent,
        to entries: inout [TranscriptEntry]
    ) {
        guard let modelStepID = event["modelStepId"]?.stringValue,
              let outcome = event["outcome"],
              let status = outcome["status"]?.stringValue
        else { return }

        guard status == "completed" else {
            let searchPrefix = scopedBlockID(
                capability: "web_search",
                sourceID: "\(modelStepID)/"
            )
            for entry in entries where entry.pending && entry.id.hasPrefix(searchPrefix) {
                entry.pending = false
                entry.title = "Web search interrupted"
                entry.tone = "warning"
                entry.sourceSequence = record.sequence
                entry.recordedAtMs = record.recordedAtMs
            }
            for entry in entries where entry.modelStepID == modelStepID && entry.pending {
                entry.pending = false
                if status == "retrying" { entry.tone = "warning" }
                entry.sourceSequence = record.sequence
                entry.recordedAtMs = record.recordedAtMs
            }
            return
        }

        let previousSnapshotIndex = entries.firstIndex(where: {
            $0.modelStepID == modelStepID && !$0.pending
                && [.reasoning, .commentary, .assistant].contains($0.kind)
        })
        entries.removeAll {
            $0.modelStepID == modelStepID
                && [.reasoning, .commentary, .assistant].contains($0.kind)
        }
        guard let content = outcome["content"]?.arrayValue else { return }

        let snapshotEntries = content.compactMap { item -> TranscriptEntry? in
            guard let outputIndex = item["outputIndex"]?.intValue,
                  let partIndex = item["partIndex"]?.intValue,
                  let phase = item["phase"]?.stringValue,
                  let text = item["text"]?.stringValue,
                  !text.isEmpty
            else { return nil }
            let kind: TranscriptEntry.Kind
            switch phase {
            case "reasoning": kind = .reasoning
            case "commentary": kind = .commentary
            case "final_answer": kind = .assistant
            default: return nil
            }
            return TranscriptEntry(
                id: snapshotID(
                    modelStepID: modelStepID,
                    phase: phase,
                    outputIndex: outputIndex,
                    partIndex: partIndex
                ),
                text: text,
                kind: kind,
                format: "plain_text",
                tone: "neutral",
                pending: false,
                modelStepID: modelStepID,
                sourceSequence: record.sequence,
                recordedAtMs: record.recordedAtMs
            )
        }
        guard !snapshotEntries.isEmpty else { return }
        let insertionIndex = min(previousSnapshotIndex ?? entries.endIndex, entries.endIndex)
        entries.insert(contentsOf: snapshotEntries, at: insertionIndex)
    }

    private func completeStream(
        text: String,
        kind: TranscriptEntry.Kind,
        messageTarget: MessageTarget?
    ) {
        completeStream(
            text: text,
            kind: kind,
            messageTarget: messageTarget,
            in: &transcript
        )
    }

    private func completeStream(
        text: String,
        kind: TranscriptEntry.Kind,
        messageTarget: MessageTarget?,
        in entries: inout [TranscriptEntry]
    ) {
        if let index = entries.lastIndex(where: { $0.pending && $0.kind == kind }) {
            entries[index].text = text
            entries[index].pending = false
            entries[index].messageTarget = messageTarget
        } else {
            appendText(text, kind: kind, messageTarget: messageTarget, to: &entries)
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

    /// Rebuilds record-owned presentation in sequence order because a replace/append pair
    /// can straddle history pages. The cached base already includes records at its cursor.
    private func mergeHistory(_ records: [RecordedEvent]) {
        for record in records { transcriptRecords[record.sequence] = record }

        var earlier: [TranscriptEntry] = []
        var rebuilt = copiedTranscript(transcriptRecordBase)
        let records = transcriptRecords.values.sorted { $0.sequence < $1.sequence }
        for record in records {
            if let baseSequence = transcriptRecordBaseSequence,
               record.sequence <= baseSequence {
                reduceHistory(record, into: &earlier)
            } else {
                reduceHistory(record, into: &rebuilt)
            }
        }
        let baseIDs = Set(transcriptRecordBase.map(\.id))
        let baseTargets = Set(transcriptRecordBase.compactMap(\.messageTarget))
        earlier.removeAll {
            baseIDs.contains($0.id)
                || $0.messageTarget.map(baseTargets.contains) == true
        }
        let previousCount = transcript.count
        rebuilt.insert(contentsOf: earlier, at: 0)
        transcript = rebuilt
        visibleTranscriptLimit += max(0, rebuilt.count - previousCount)
    }

    private func copiedTranscript(_ entries: [TranscriptEntry]) -> [TranscriptEntry] {
        entries.map { entry in
            TranscriptEntry(
                id: entry.id,
                text: entry.text,
                kind: entry.kind,
                capability: entry.capability,
                role: entry.role,
                update: entry.update,
                title: entry.title,
                symbol: entry.symbol,
                group: entry.group,
                format: entry.format,
                tone: entry.tone,
                pending: entry.pending,
                modelStepID: entry.modelStepID,
                sourceSequence: entry.sourceSequence,
                recordedAtMs: entry.recordedAtMs,
                messageTarget: entry.messageTarget,
                files: entry.files
            )
        }
    }

    private func reduceHistory(
        _ record: RecordedEvent,
        into entries: inout [TranscriptEntry],
        recordID: String? = nil
    ) {
        let event = record.event.msg
        let type = event["type"]?.stringValue ?? "unknown"
        for (index, block) in record.blocks.enumerated() {
            apply(
                block,
                sequence: record.sequence,
                blockIndex: index,
                recordedAtMs: record.recordedAtMs,
                recordID: recordID,
                to: &entries
            )
        }

        switch type {
        case "user_message":
            let attachments = event["attachments"]?.arrayValue?.compactMap {
                try? SessionFileReference(json: $0)
            } ?? []
            appendText(
                event["message"]?.stringValue,
                kind: .user,
                id: "event:\(recordID ?? String(record.sequence)):user",
                sourceSequence: record.sequence,
                recordedAtMs: record.recordedAtMs,
                messageTarget: messageTarget(from: event),
                files: attachments,
                to: &entries
            )
        case "agent_message_content_delta", "agent_reasoning_content_delta":
            guard let modelStepID = event["modelStepId"]?.stringValue else { return }
            let reasoning = type == "agent_reasoning_content_delta"
            let commentary = event["phase"]?.stringValue == "commentary"
            let phase = reasoning ? "reasoning" : (commentary ? "commentary" : "final_answer")
            let id = streamID(modelStepID: modelStepID, phase: phase)
            let kind: TranscriptEntry.Kind = reasoning
                ? .reasoning
                : (commentary ? .commentary : .assistant)
            let delta = event["delta"]?.stringValue ?? ""
            guard !delta.isEmpty else { return }
            if let index = entries.lastIndex(where: { $0.id == id }) {
                entries[index].text.append(delta)
                entries[index].sourceSequence = record.sequence
                entries[index].recordedAtMs = record.recordedAtMs
            } else {
                entries.append(TranscriptEntry(
                    id: id,
                    text: delta,
                    kind: kind,
                    format: "plain_text",
                    tone: "neutral",
                    pending: true,
                    modelStepID: modelStepID,
                    sourceSequence: record.sequence,
                    recordedAtMs: record.recordedAtMs
                ))
            }
        case "model_step_completed":
            applyModelStepCompletion(event, record: record, to: &entries)
        case "agent_message":
            let kind: TranscriptEntry.Kind = event["phase"]?.stringValue == "commentary"
                ? .commentary
                : .assistant
            if let modelStepID = event["modelStepId"]?.stringValue,
               let index = entries.lastIndex(where: {
                   $0.modelStepID == modelStepID && $0.kind == kind && !$0.pending
               }) {
                entries[index].messageTarget = messageTarget(from: event)
            } else {
                completeStream(
                    text: event["message"]?.stringValue ?? "",
                    kind: kind,
                    messageTarget: messageTarget(from: event),
                    in: &entries
                )
            }
        case "task_complete":
            for entry in entries where entry.pending { entry.pending = false }
        case "turn_aborted":
            for entry in entries where entry.pending { entry.pending = false }
        default:
            break
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

    private func startNextSessionFileUpload() {
        guard connectionState.isReady,
              activeSessionFileUpload == nil,
              sessionFileUploadRequests.isEmpty,
              let sessionID = selectedSessionID,
              let index = composerAttachments.firstIndex(where: {
                  if case .queued = $0.state { return true }
                  return false
              }),
              sessionFileData[composerAttachments[index].id] != nil
        else { return }

        let item = composerAttachments[index]
        composerAttachments[index].state = .uploading
        let id = requestID("session-file-begin")
        sessionFileUploadRequests[id] = .begin(localID: item.id)
        transmit(.beginSessionFileUpload(
            requestID: id,
            sessionID: sessionID,
            name: item.name,
            size: item.size,
            mediaType: item.mediaType
        )) { [weak self] message in
            self?.failSessionFileUploadRequest(id, message: message, showsToast: false)
        }
    }

    private func handleSessionFileUploadReady(
        requestID: String,
        sessionID: String,
        uploadID: String,
        maxChunkBytes: Int
    ) {
        guard let request = sessionFileUploadRequests[requestID] else { return }
        guard case .begin(let localID) = request else {
            return failAttachment(request.localID, message: "The gateway returned an invalid upload.")
        }
        guard sessionID == selectedSessionID,
              !uploadID.isEmpty,
              maxChunkBytes > 0,
              maxChunkBytes <= maximumGatewayFrameBytes
        else { return failAttachment(localID, message: "The gateway returned an invalid upload.") }
        sessionFileUploadRequests.removeValue(forKey: requestID)
        activeSessionFileUpload = ActiveSessionFileUpload(
            localID: localID,
            sessionID: sessionID,
            uploadID: uploadID,
            maxChunkBytes: min(maxChunkBytes, 256 * 1024)
        )
        sendNextSessionFileChunk(localID: localID, offset: 0)
    }

    private func handleSessionFileUploadChunkAccepted(
        requestID: String,
        sessionID: String,
        uploadID: String,
        nextOffset: Int64
    ) {
        guard let request = sessionFileUploadRequests[requestID] else { return }
        guard case .chunk(let localID, let expectedNextOffset) = request else {
            return failAttachment(request.localID, message: "The gateway returned an invalid upload.")
        }
        guard let upload = activeSessionFileUpload,
              upload.localID == localID,
              upload.sessionID == sessionID,
              upload.uploadID == uploadID
        else {
            return failAttachment(localID, message: "The gateway returned an invalid upload.")
        }
        guard nextOffset == expectedNextOffset else {
            return failAttachment(localID, message: "The gateway returned an invalid upload offset.")
        }
        sessionFileUploadRequests.removeValue(forKey: requestID)
        sendNextSessionFileChunk(localID: localID, offset: nextOffset)
    }

    private func sendNextSessionFileChunk(localID: UUID, offset: Int64) {
        guard let upload = activeSessionFileUpload,
              upload.localID == localID,
              let data = sessionFileData[localID],
              offset >= 0,
              let start = Int(exactly: offset),
              start <= data.count
        else {
            failAttachment(localID, message: "The gateway returned an invalid upload offset.")
            return
        }
        guard start < data.count else {
            let id = requestID("session-file-finish")
            sessionFileUploadRequests[id] = .finish(localID: localID)
            transmit(.finishSessionFileUpload(
                requestID: id,
                sessionID: upload.sessionID,
                uploadID: upload.uploadID
            )) { [weak self] message in
                self?.failSessionFileUploadRequest(id, message: message, showsToast: false)
            }
            return
        }

        let end = min(start + upload.maxChunkBytes, data.count)
        let id = requestID("session-file-chunk")
        sessionFileUploadRequests[id] = .chunk(
            localID: localID,
            expectedNextOffset: Int64(end)
        )
        transmit(.uploadSessionFileChunk(
            requestID: id,
            sessionID: upload.sessionID,
            uploadID: upload.uploadID,
            offset: offset,
            data: Data(data[start..<end])
        )) { [weak self] message in
            self?.failSessionFileUploadRequest(id, message: message, showsToast: false)
        }
    }

    private func handleSessionFileUploadCompleted(
        requestID: String,
        sessionID: String,
        file: SessionFileReference
    ) {
        guard let request = sessionFileUploadRequests[requestID] else { return }
        guard case .finish(let localID) = request else {
            return failAttachment(request.localID, message: "The gateway returned an invalid file.")
        }
        guard sessionID == selectedSessionID,
              activeSessionFileUpload?.localID == localID,
              activeSessionFileUpload?.sessionID == sessionID,
              let index = composerAttachments.firstIndex(where: { $0.id == localID }),
              composerAttachments[index].name == file.name,
              composerAttachments[index].size == file.size,
              composerAttachments[index].mediaType == file.mediaType
        else {
            return failAttachment(localID, message: "The gateway returned an invalid file.")
        }
        sessionFileUploadRequests.removeValue(forKey: requestID)
        composerAttachments[index].state = .uploaded(file)
        sessionFileData[localID] = nil
        activeSessionFileUpload = nil
        upsertSessionUpload(file)
        startNextSessionFileUpload()
    }

    @discardableResult
    private func failSessionFileUploadRequest(
        _ requestID: String,
        message: String,
        showsToast: Bool = true
    ) -> Bool {
        guard let request = sessionFileUploadRequests.removeValue(forKey: requestID) else {
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
        sessionFileUploadRequests = sessionFileUploadRequests.filter { _, request in
            request.localID != localID
        }
        if activeSessionFileUpload?.localID == localID { activeSessionFileUpload = nil }
        if let index = composerAttachments.firstIndex(where: { $0.id == localID }) {
            composerAttachments[index].state = .failed(message)
        }
        if showsToast { showToast(message, tone: .error) }
        startNextSessionFileUpload()
    }

    private func upsertSessionUpload(_ file: SessionFileReference) {
        if let index = sessionUploads.firstIndex(where: { $0.id == file.id }) {
            sessionUploads[index] = file
        } else {
            sessionUploads.append(file)
        }
    }

    private func discardComposerAttachments() {
        attachmentImportGeneration = UUID()
        composerAttachments.removeAll()
        sessionFileData.removeAll()
    }

    private func discardPendingComposerAttachments() {
        attachmentImportGeneration = UUID()
        composerAttachments.removeAll { item in
            if case .uploaded = item.state { return false }
            return true
        }
        sessionFileData.removeAll()
    }

    private func handleSessionFileChunk(
        requestID: String,
        sessionID: String,
        fileID: String,
        offset: Int64,
        data: Data,
        nextOffset: Int64?
    ) {
        guard var download = sessionFileDownload,
              download.requestID == requestID
        else { return }
        sessionFileDownload = nil
        guard download.sessionID == sessionID,
              download.file.id == fileID,
              offset == Int64(download.data.count),
              data.count <= 256 * 1024,
              Int64(download.data.count + data.count) <= download.file.size
        else {
            isLoadingFilePresentation = false
            showToast("The gateway returned an invalid session file.", tone: .error)
            return
        }
        download.data.append(data)
        if let nextOffset {
            guard nextOffset == Int64(download.data.count), nextOffset > offset else {
                isLoadingFilePresentation = false
                showToast("The gateway returned an invalid session file offset.", tone: .error)
                return
            }
            let id = self.requestID("session-file-read")
            download.requestID = id
            sessionFileDownload = download
            transmit(.readSessionFile(
                requestID: id,
                sessionID: sessionID,
                fileID: fileID,
                offset: nextOffset,
                maxBytes: 256 * 1024
            )) { [weak self] message in
                guard self?.sessionFileDownload?.requestID == id else { return }
                self?.sessionFileDownload = nil
                self?.isLoadingFilePresentation = false
                self?.showToast(message, tone: .error)
            }
            return
        }

        guard Int64(download.data.count) == download.file.size else {
            isLoadingFilePresentation = false
            showToast("The downloaded file is incomplete.", tone: .error)
            return
        }
        finishFilePresentation(
            download.data,
            name: download.file.name,
            generation: download.generation,
            purpose: download.purpose,
            allowsTextPreview: !download.file.mediaType.lowercased().hasPrefix("image/")
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
        guard var download = workspaceFilePreviewDownload,
              download.requestID == requestID
        else { return }
        workspaceFilePreviewDownload = nil
        guard download.sessionID == sessionID,
              download.file.path == path,
              offset == UInt64(download.data.count),
              data.count <= 256 * 1024,
              offset <= download.file.size,
              UInt64(data.count) <= download.file.size - offset
        else {
            isLoadingFilePresentation = false
            showToast("The gateway returned an invalid workspace file.", tone: .error)
            return
        }
        download.data.append(data)
        if let nextOffset {
            guard nextOffset == UInt64(download.data.count), nextOffset > offset else {
                isLoadingFilePresentation = false
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
                self?.isLoadingFilePresentation = false
                self?.showToast(message, tone: .error)
            }
            return
        }

        guard UInt64(download.data.count) == download.file.size else {
            isLoadingFilePresentation = false
            showToast("The downloaded workspace file is incomplete.", tone: .error)
            return
        }
        finishFilePresentation(
            download.data,
            name: URL(fileURLWithPath: download.file.path).lastPathComponent,
            generation: download.generation,
            purpose: .preview,
            allowsTextPreview: true
        )
    }

    private func finishFilePresentation(
        _ data: Data,
        name: String,
        generation: UUID,
        purpose: SessionFileDownloadPurpose,
        allowsTextPreview: Bool
    ) {
        Task { [weak self] in
            if purpose == .preview, allowsTextPreview {
                let contents = await Self.utf8Text(in: data)
                guard let self, self.filePresentationGeneration == generation else { return }
                if let contents {
                    self.textFilePreview = TextFilePreview(
                        id: generation,
                        name: name,
                        contents: contents
                    )
                    self.isLoadingFilePresentation = false
                    return
                }
            }
            do {
                let file = try await Self.writeTemporarySessionFile(data, name: name)
                guard let self else {
                    await Self.removePreviewDirectory(file.directory)
                    return
                }
                guard self.filePresentationGeneration == generation else {
                    await Self.removePreviewDirectory(file.directory)
                    return
                }
                let previousDirectory = self.previewTemporaryDirectory
                self.previewTemporaryDirectory = file.directory
                if purpose == .share {
                    self.sessionFileShareItem = SessionFileShareItem(
                        id: generation,
                        name: name,
                        url: file.url
                    )
                } else {
                    self.previewURL = file.url
                }
                self.isLoadingFilePresentation = false
                if let previousDirectory {
                    Task { await Self.removePreviewDirectory(previousDirectory) }
                }
            } catch {
                guard let self, self.filePresentationGeneration == generation else { return }
                self.isLoadingFilePresentation = false
                self.showToast(error.localizedDescription, tone: .error)
            }
        }
    }

    private nonisolated static func utf8Text(in data: Data) async -> String? {
        guard data.count <= maximumHighlightedPreviewBytes else { return nil }
        return await Task.detached(priority: .userInitiated) {
            guard let text = String(data: data, encoding: .utf8) else { return nil }
            let allowedControls: Set<Unicode.Scalar> = ["\t", "\n", "\r"]
            guard !text.unicodeScalars.contains(where: {
                CharacterSet.controlCharacters.contains($0) && !allowedControls.contains($0)
            }) else { return nil }
            return text
        }.value
    }

    private nonisolated static func writeTemporarySessionFile(
        _ data: Data,
        name: String
    ) async throws -> TemporarySessionFile {
        try await Task.detached(priority: .userInitiated) {
            let directory = URL.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let candidateExtension = URL(fileURLWithPath: name).pathExtension
            let safeExtension = candidateExtension.utf8.count <= 16
                && candidateExtension.unicodeScalars.allSatisfy(CharacterSet.alphanumerics.contains)
                ? candidateExtension
                : ""
            let candidateName = URL(fileURLWithPath: name).lastPathComponent
            let safeName = candidateName.utf8.count <= 255
                && candidateName != "."
                && candidateName != ".."
                && !candidateName.unicodeScalars.contains(where: {
                    CharacterSet.controlCharacters.contains($0) || $0 == "/" || $0 == "\\" || $0 == ":"
                })
                ? candidateName
                : ""
            let url: URL
            if !safeName.isEmpty {
                url = directory.appending(path: safeName)
            } else if safeExtension.isEmpty {
                url = directory.appending(path: "file")
            } else {
                url = directory.appending(path: "file").appendingPathExtension(safeExtension)
            }
            try data.write(to: url, options: [.atomic, .completeFileProtection])
            return TemporarySessionFile(directory: directory, url: url)
        }.value
    }

    private nonisolated static func removePreviewDirectory(_ directory: URL) async {
        await Task.detached(priority: .utility) {
            try? FileManager.default.removeItem(at: directory)
        }.value
    }

    private func widgets(in slot: FrontendSlot) -> [MountedWidget] {
        mountedWidgets.filter { $0.widget.slot == slot }
    }

    private func requestID(_ prefix: String) -> String {
        "\(prefix)-\(UUID().uuidString.lowercased())"
    }

    private func enqueueTranscriptIO(
        _ operation: @escaping @MainActor @Sendable () async -> Void
    ) {
        let previous = transcriptIOTask
        transcriptIOTask = Task {
            await previous?.value
            await operation()
        }
    }

    private func scheduleComposerDraftSave() {
        guard !suppressesComposerDraftSave,
              !isLoadingComposerDraft,
              !isLoadingComposerEditRecovery,
              let owner = composerDraftOwner
        else { return }
        composerDraftSaveTask?.cancel()
        if var pending = pendingWidgetEdit,
           pending.owner == owner,
           pending.recovery.phase == .editing {
            guard composer.utf8.count <= maximumComposerBytes else { return }
            pending.recovery.editedInput = composer
            pendingWidgetEdit = pending
            let recovery = pending.recovery
            composerDraftSaveTask = Task { [weak self] in
                do {
                    try await Task.sleep(for: .milliseconds(400))
                } catch {
                    return
                }
                guard let self,
                      self.pendingWidgetEdit?.owner == owner,
                      self.pendingWidgetEdit?.recovery.phase == .editing,
                      self.pendingWidgetEdit?.recovery.editedInput == recovery.editedInput
                else { return }
                self.composerDraftSaveTask = nil
                self.enqueueComposerEditRecoverySave(recovery, owner: owner)
            }
            return
        }
        guard stashedComposerDraft == nil else { return }
        let text = composer
        composerDraftSaveTask = Task { [weak self] in
            do {
                try await Task.sleep(for: .milliseconds(400))
            } catch {
                return
            }
            guard let self, owner == composerDraftOwner else { return }
            composerDraftSaveTask = nil
            enqueueComposerDraftSave(text, owner: owner)
        }
    }

    private func flushComposerDraft() {
        composerDraftSaveTask?.cancel()
        composerDraftSaveTask = nil
        guard stashedComposerDraft == nil, let owner = composerDraftOwner else { return }
        enqueueComposerDraftSave(composer, owner: owner)
    }

    private func enqueueComposerDraftSave(_ text: String, owner: ComposerDraftOwner) {
        let previous = composerDraftIOTask
        let store = store
        composerDraftIOTask = Task {
            await previous?.value
            await store.saveComposerDraft(
                text,
                accountID: owner.accountID,
                sessionID: owner.sessionID
            )
        }
    }

    private func enqueueComposerEditRecoverySave(
        _ recovery: ComposerEditRecovery,
        owner: ComposerDraftOwner,
        completion: ((Result<Void, Error>) -> Void)? = nil
    ) {
        let previous = composerDraftIOTask
        let store = store
        composerDraftIOTask = Task {
            await previous?.value
            do {
                try await store.saveComposerEditRecovery(
                    recovery,
                    accountID: owner.accountID,
                    sessionID: owner.sessionID
                )
                completion?(.success(()))
            } catch {
                completion?(.failure(error))
            }
        }
    }

    private func enqueueComposerEditRecoveryRemoval(owner: ComposerDraftOwner) {
        let previous = composerDraftIOTask
        let store = store
        composerDraftIOTask = Task {
            await previous?.value
            try? await store.removeComposerEditRecovery(
                accountID: owner.accountID,
                sessionID: owner.sessionID
            )
        }
    }

    private func prepareComposerEditRecovery(for owner: ComposerDraftOwner) {
        guard composerDraftOwner == owner else { return }
        if pendingWidgetEdit?.owner == owner {
            if replayRequestID == nil { reconcileComposerEditRecovery() }
            return
        }
        let generation = UUID()
        composerEditRecoveryGeneration = generation
        isLoadingComposerEditRecovery = true
        let previous = composerDraftIOTask
        let store = store
        composerDraftIOTask = Task { [weak self] in
            await previous?.value
            let recovery = await store.loadComposerEditRecovery(
                accountID: owner.accountID,
                sessionID: owner.sessionID
            )
            guard let self,
                  self.composerEditRecoveryGeneration == generation,
                  self.composerDraftOwner == owner
            else { return }
            self.isLoadingComposerEditRecovery = false
            self.pendingWidgetEdit = recovery.map {
                PendingWidgetEdit(owner: owner, recovery: $0)
            }
            if self.replayRequestID == nil { self.reconcileComposerEditRecovery() }
        }
    }

    private func observeReplayCompletion(_ buffered: BufferedAgentEvent) {
        guard replayRequestID != nil else { return }
        let event = buffered.record.event
        let type = event.msg["type"]?.stringValue
        if let submissionID = event.submissionId,
           type == "user_message"
               || (type == "frontend"
                   && event.msg["frontendType"]?.stringValue == "widget"),
           replayCompletionSubmissionIDs.count < maximumObservedReplaySubmissions
               || replayCompletionSubmissionIDs.contains(submissionID) {
            replayCompletionSubmissionIDs.insert(submissionID)
        }

        var messages: [ReplayUserMessage] = []
        if type == "user_message", let text = event.msg["message"]?.stringValue {
            let sequence = messageTarget(from: event.msg)?.checkpointSequence
                ?? buffered.record.sequence
            messages.append(ReplayUserMessage(sequence: sequence, text: text))
        }
        guard !messages.isEmpty else { return }
        replayUserMessages.append(contentsOf: messages.suffix(maximumObservedReplaySubmissions))
        if replayUserMessages.count > maximumObservedReplaySubmissions {
            replayUserMessages.removeFirst(
                replayUserMessages.count - maximumObservedReplaySubmissions
            )
        }
    }

    private func reconcileComposerEditRecovery() {
        guard replayRequestID == nil,
              !isLoadingComposerEditRecovery
        else { return }
        defer {
            replayCompletionSubmissionIDs.removeAll(keepingCapacity: true)
            replayUserMessages.removeAll(keepingCapacity: true)
            completedComposerEditReplay = false
        }
        guard let pending = pendingWidgetEdit,
              pending.owner == composerDraftOwner
        else { return }
        let matchingWidgetInput = mountedWidgets.first(where: {
            $0.capability == pending.recovery.capability
                && $0.widget.id == pending.recovery.widgetID
        })?.widget.action?.capabilityInput
        let renderedEditedInput: Bool = if let baseline = pending.recovery.submissionBaselineSequence {
            transcript.contains {
                $0.kind == .user
                    && $0.text == pending.recovery.editedInput
                    && ($0.messageTarget?.checkpointSequence ?? 0) > baseline
            } || replayUserMessages.contains {
                $0.sequence > baseline && $0.text == pending.recovery.editedInput
            }
        } else {
            false
        }
        switch pending.recovery.phase {
        case .removingQueuedInput where matchingWidgetInput == pending.recovery.originalInput:
            completeComposerEditRecovery(pending)
        case .submitting where matchingWidgetInput == pending.recovery.editedInput
            || replayCompletionSubmissionIDs.contains(pending.recovery.requestID)
            || renderedEditedInput:
            completeComposerEditRecovery(pending)
        case .removingQueuedInput, .editing:
            restoreComposerEditMode(pending)
        case .submitting where completedComposerEditReplay:
            restoreComposerEditMode(pending)
        case .submitting:
            break
        case .completed:
            completeComposerEditRecovery(pending)
        }
    }

    private func restoreComposerEditMode(requestID: String) {
        guard let pending = pendingWidgetEdit,
              pending.recovery.requestID == requestID,
              pending.recovery.phase == .submitting
        else { return }
        restoreComposerEditMode(pending)
    }

    private func restoreComposerEditMode(_ current: PendingWidgetEdit) {
        var pending = current
        pending.recovery.phase = .editing
        pendingWidgetEdit = pending
        stashedComposerDraft = pending.recovery.displacedDraft
        suppressesComposerDraftSave = true
        composer = pending.recovery.editedInput
        suppressesComposerDraftSave = false
        composerFocusRequest &+= 1
        enqueueComposerEditRecoverySave(pending.recovery, owner: pending.owner)
    }

    private func rejectComposerEdit(requestID: String) {
        guard let pending = pendingWidgetEdit,
              pending.recovery.requestID == requestID
        else { return }
        switch pending.recovery.phase {
        case .removingQueuedInput:
            completeComposerEditRecovery(pending)
        case .submitting:
            restoreComposerEditMode(pending)
        case .editing, .completed:
            break
        }
    }

    private func completeSubmittedComposerEdit(requestID: String) {
        guard let pending = pendingWidgetEdit,
              pending.recovery.requestID == requestID,
              pending.recovery.phase == .submitting
        else { return }
        completeComposerEditRecovery(pending)
    }

    private func completeComposerEditRecovery(_ current: PendingWidgetEdit) {
        guard let pending = pendingWidgetEdit,
              pending.owner == current.owner,
              pending.recovery.requestID == current.recovery.requestID
        else { return }
        var completed = pending
        completed.recovery.phase = .completed
        pendingWidgetEdit = completed
        enqueueComposerEditRecoverySave(completed.recovery, owner: completed.owner) { [weak self] result in
            guard let self,
                  self.pendingWidgetEdit?.owner == completed.owner,
                  self.pendingWidgetEdit?.recovery.requestID == completed.recovery.requestID,
                  self.pendingWidgetEdit?.recovery.phase == .completed
            else { return }
            switch result {
            case .success:
                self.pendingWidgetEdit = nil
                self.stashedComposerDraft = nil
                self.cacheSelectedTranscript()
            case .failure(let error):
                self.showToast(error.localizedDescription, tone: .error)
            }
        }
    }

    private func changeComposerDraftOwner(to owner: ComposerDraftOwner?) {
        guard owner != composerDraftOwner else { return }
        composerDraftSaveTask?.cancel()
        composerDraftSaveTask = nil
        let previousOwner = composerDraftOwner
        if var pending = pendingWidgetEdit,
           pending.owner == previousOwner,
           pending.recovery.phase == .editing,
           composer.utf8.count <= maximumComposerBytes {
            pending.recovery.editedInput = composer
            pendingWidgetEdit = pending
            enqueueComposerEditRecoverySave(pending.recovery, owner: pending.owner)
        }
        let previousText = pendingWidgetEdit?.recovery.displacedDraft ?? composer
        pendingWidgetEdit = nil
        stashedComposerDraft = nil
        composerEditRecoveryGeneration = UUID()
        isLoadingComposerEditRecovery = false
        let previousIO = composerDraftIOTask
        let generation = UUID()
        composerDraftGeneration = generation
        composerDraftOwner = owner
        isLoadingComposerDraft = owner != nil
        suppressesComposerDraftSave = true
        composer = previousOwner == nil ? previousText : ""
        suppressesComposerDraftSave = false
        let store = store
        composerDraftIOTask = Task { [weak self] in
            await previousIO?.value
            if let previousOwner {
                await store.saveComposerDraft(
                    previousText,
                    accountID: previousOwner.accountID,
                    sessionID: previousOwner.sessionID
                )
            }
            guard let owner else { return }
            let restored = await store.loadComposerDraft(
                accountID: owner.accountID,
                sessionID: owner.sessionID
            )
            guard let self,
                  composerDraftGeneration == generation,
                  composerDraftOwner == owner
            else { return }
            suppressesComposerDraftSave = true
            if composer.isEmpty {
                composer = restored
            } else if !restored.isEmpty {
                composer = "\(restored)\n\n\(composer)"
            }
            suppressesComposerDraftSave = false
            isLoadingComposerDraft = false
            scheduleComposerDraftSave()
        }
    }

    private func discardComposerDraft() {
        composerDraftSaveTask?.cancel()
        composerDraftSaveTask = nil
        invalidateComposerEditRecovery()
        composerDraftGeneration = UUID()
        composerDraftOwner = nil
        isLoadingComposerDraft = false
        suppressesComposerDraftSave = true
        composer = ""
        suppressesComposerDraftSave = false
    }

    private func invalidateComposerEditRecovery(for owner: ComposerDraftOwner? = nil) {
        if let owner {
            guard pendingWidgetEdit?.owner == owner || composerDraftOwner == owner else { return }
        }
        composerDraftSaveTask?.cancel()
        composerDraftSaveTask = nil
        pendingWidgetEdit = nil
        stashedComposerDraft = nil
        composerEditRecoveryGeneration = UUID()
        isLoadingComposerEditRecovery = false
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
        let available = max(0, maximumSessionFileReferences - composerAttachments.count)
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
        connectionGeneration = UUID()
        transcriptLoadGeneration = UUID()
        eventTask = nil
        connectionState = .failed(message)
        sessionFileUploadRequests.removeAll()
        activeSessionFileUpload = nil
        sessionUploadsRequestID = nil
        isLoadingSessionUploads = false
        artifactListRequestID = nil
        isLoadingArtifacts = false
        gitDiffRequestID = nil
        isLoadingGitDiff = false
        workspaceFilesRequestID = nil
        isLoadingWorkspaceFiles = false
        discardPendingComposerAttachments()
        discardFilePresentation()
        restorePendingDrafts()
        if pendingPairingAccount != nil { pairingError = message }
        if reconnectAttempt == 0 { showToast(message, tone: .error) }
        scheduleReconnect()
    }

    private func scheduleReconnect() {
        guard reconnectTask == nil,
              !automaticReconnectBlocked,
              pendingPairingAccount == nil,
              let account = selectedAccount
        else { return }
        guard !appIsInBackground else {
            reconnectsOnActivation = true
            return
        }
        let attempt = reconnectAttempt
        reconnectAttempt += 1
        let generation = connectionGeneration
        reconnectTask = Task { [weak self] in
            guard let self else { return }
            do {
                try await Task.sleep(for: reconnectDelay(attempt))
            } catch {
                return
            }
            guard !Task.isCancelled,
                  generation == connectionGeneration,
                  selectedAccountID == account.id
            else { return }
            reconnectTask = nil
            connect(to: account, retrying: true)
        }
    }

    private func cancelReconnect() {
        reconnectTask?.cancel()
        reconnectTask = nil
    }

    @discardableResult
    private func resetGatewayState(
        preservingDrafts: Bool,
        preservingSession: Bool = false
    ) -> UUID {
        if !preservingSession { changeComposerDraftOwner(to: nil) }
        if preservingSession { flushStreamDeltas() }
        connectionGeneration = UUID()
        transcriptLoadGeneration = UUID()
        eventTask?.cancel()
        eventTask = nil
        if !preservingSession {
            latestSequence = nil
        }
        sessionOpenCursor = nil
        replayRequestID = nil
        replaySnapshotSequence = nil
        historyRequestID = nil
        isLoadingEarlierHistory = false
        if !preservingSession {
            nextHistoryBeforeSequence = nil
            visibleTranscriptLimit = 300
        }
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
        pendingDeletedSessionID = nil
        if preservingSession {
            for sessionID in Array(pendingChatTitles.keys) {
                pendingChatTitles[sessionID]?.renameRequestID = nil
            }
        }
        sessionToRestoreID = nil
        configRequestID = nil
        defaultConfigRequestID = nil
        submittedDefaultAgentDraft = nil
        chatAgentApplyState = .idle
        defaultAgentApplyState = .idle
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
            sessionUploadsRequestID = nil
            isLoadingSessionUploads = false
            artifactListRequestID = nil
            isLoadingArtifacts = false
            sessionFileUploadRequests.removeAll()
            activeSessionFileUpload = nil
            discardFilePresentation()
        }
        if !preservingSession {
            chatTitleTasks.values.forEach { $0.cancel() }
            chatTitleTasks.removeAll()
            titleEligibleSessionIDs.removeAll()
            pendingChatTitles.removeAll()
            sessions = []
            gatewayMachineName = ""
            selectedSessionID = nil
            sessionToRename = nil
            sessionRenameDraft = ""
            sessionToDelete = nil
            unreadSessionIDs.removeAll()
            profile = nil
            modelChoices = []
            modelProviders = [:]
            middlewareFeatures = []
            providerStatuses = []
            defaultAgentSnapshot = nil
            defaultAgentDraft = nil
            setupProviderDraft = nil
        }
        providerAPIKey = ""
        providerModelIDsText = ""
        providerReasoningEffortsText = ""
        providerActionState = .idle
        credentialRequestID = nil
        providerLoginRequestID = nil
        providerRegistrationRequestID = nil
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
        filesInspectorTab = .unstaged
        gitBranchRequestID = nil
        discardComposerAttachments()
        sessionUploads = []
        sessionUploadsRequestID = nil
        isLoadingSessionUploads = false
        artifacts = []
        artifactsTruncated = false
        artifactListRequestID = nil
        isLoadingArtifacts = false
        sessionFileUploadRequests.removeAll()
        activeSessionFileUpload = nil
        discardFilePresentation()
        selectedModelRoute = ""
        contributions = []
        agentSnapshot = nil
        agentDraft = nil
        chatAgentApplyState = .idle
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
        transcriptRecordBase = []
        transcriptRecordBaseSequence = nil
        transcriptRecords.removeAll(keepingCapacity: true)
        replayCompletionSubmissionIDs.removeAll(keepingCapacity: true)
        replayUserMessages.removeAll(keepingCapacity: true)
        completedComposerEditReplay = false
        historyRequestID = nil
        isLoadingEarlierHistory = false
        nextHistoryBeforeSequence = nil
        visibleTranscriptLimit = 300
        activeTurnID = nil
        activeOperation = nil
        awaitsSteeringDelivery = false
        runStats = RunStats()
        contextTokens = 0
        modelContextWindow = nil
        pendingApproval = nil
        approvalRequestID = nil
        pendingPicker = nil
        mountedWidgets = []
        previews = []
        presentedPreview = nil
        previewSelections.removeAll()
        previewPageRequestID = nil
        isLoadingPreviewPage = false
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
