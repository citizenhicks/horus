import Foundation

let gatewayProtocolVersion = 1
let maximumGatewayFrameBytes = 2 * 1024 * 1024
let maximumComposerBytes = 1024 * 1024

enum GatewayWireError: LocalizedError, Equatable {
    case invalidEndpoint(String)
    case insecureRemoteEndpoint
    case unsupportedVersion(Int)
    case oversizedFrame(Int)
    case invalidFrame(String)
    case disconnected

    var errorDescription: String? {
        switch self {
        case .invalidEndpoint(let message): message
        case .insecureRemoteEndpoint:
            "Plaintext gateway connections are allowed only on this device. Use tls:// for remote gateways."
        case .unsupportedVersion(let version): "Gateway protocol version \(version) is not supported."
        case .oversizedFrame(let size): "Gateway frame is too large (\(size) bytes)."
        case .invalidFrame(let message): "Invalid gateway frame: \(message)"
        case .disconnected: "The gateway disconnected."
        }
    }
}

struct GatewayEndpoint: Hashable, Codable, Sendable {
    let rawValue: String

    init(_ rawValue: String) throws {
        let trimmed = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let components = URLComponents(string: trimmed),
              let scheme = components.scheme?.lowercased(),
              let parsedHost = components.host,
              let port = components.port,
              (1...65_535).contains(port),
              components.user == nil,
              components.password == nil,
              components.query == nil,
              components.fragment == nil,
              components.path.isEmpty || components.path == "/"
        else {
            throw GatewayWireError.invalidEndpoint("Use tcp://host:port or tls://host:port.")
        }
        guard scheme == "tcp" || scheme == "tls" else {
            throw GatewayWireError.invalidEndpoint("The endpoint scheme must be tcp:// or tls://.")
        }
        let host = Self.normalized(host: parsedHost)
        guard !host.isEmpty else {
            throw GatewayWireError.invalidEndpoint("Use tcp://host:port or tls://host:port.")
        }
        if scheme == "tcp" && !Self.isLoopback(host) {
            throw GatewayWireError.insecureRemoteEndpoint
        }
        self.rawValue = "\(scheme)://\(Self.formatted(host: host)):\(port)"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        try self.init(container.decode(String.self, forKey: .rawValue))
    }

    var usesTLS: Bool { rawValue.hasPrefix("tls://") }

    var host: String {
        Self.normalized(host: URLComponents(string: rawValue)?.host ?? "")
    }

    var port: UInt16 {
        UInt16(URLComponents(string: rawValue)?.port ?? 0)
    }

    var displayName: String {
        "\(host):\(port)"
    }

    private static func isLoopback(_ host: String) -> Bool {
        host.caseInsensitiveCompare("localhost") == .orderedSame
            || host == "127.0.0.1"
            || host == "::1"
    }

    private static func formatted(host: String) -> String {
        host.contains(":") ? "[\(host)]" : host
    }

    private static func normalized(host: String) -> String {
        guard host.first == "[", host.last == "]" else { return host }
        return String(host.dropFirst().dropLast())
    }
}

struct GatewayAccount: Identifiable, Hashable, Codable, Sendable {
    let id: UUID
    var endpoint: GatewayEndpoint
    var displayName: String

    init(id: UUID = UUID(), endpoint: GatewayEndpoint, displayName: String? = nil) {
        self.id = id
        self.endpoint = endpoint
        self.displayName = displayName ?? endpoint.displayName
    }
}

struct Submission: Encodable, Sendable {
    let id: String
    let op: AgentOperation
}

enum AgentOperation: Codable, Sendable {
    case userInput(text: String)
    case activeInput(operation: String, turnID: String, text: String)
    case interrupt(turnID: String)
    case execApproval(id: String, decision: ReviewDecision)
    case capabilityCommand(capability: String, command: String, arguments: String)
    case setModel(route: String)
    case resumeSession(sessionID: String)
    case unknown(JSONValue)

    init(from decoder: Decoder) throws {
        try self.init(json: JSONValue(from: decoder))
    }

    init(json value: JSONValue) throws {
        guard let type = value["type"]?.stringValue else {
            throw GatewayWireError.invalidFrame("agent operation has no type")
        }
        func required(_ key: String) throws -> String {
            guard let string = value[key]?.stringValue else {
                throw GatewayWireError.invalidFrame("\(type) has no \(key)")
            }
            return string
        }
        switch type {
        case "user_input":
            self = .userInput(text: try required("text"))
        case "active_input":
            self = .activeInput(
                operation: try required("operation"),
                turnID: try required("turnId"),
                text: try required("text")
            )
        case "interrupt":
            self = .interrupt(turnID: try required("turnId"))
        case "exec_approval":
            guard let decision = value["decision"] else {
                throw GatewayWireError.invalidFrame("exec_approval has no decision")
            }
            self = .execApproval(
                id: try required("id"),
                decision: try ReviewDecision(json: decision)
            )
        case "capability_command":
            self = .capabilityCommand(
                capability: try required("capability"),
                command: try required("command"),
                arguments: try required("arguments")
            )
        case "set_model":
            self = .setModel(route: try required("route"))
        case "resume_session":
            self = .resumeSession(sessionID: try required("sessionId"))
        default:
            self = .unknown(value)
        }
    }

    func encode(to encoder: Encoder) throws {
        if case .unknown(let value) = self {
            try value.encode(to: encoder)
            return
        }
        var container = encoder.container(keyedBy: DynamicCodingKey.self)
        switch self {
        case .userInput(let text):
            try container.encode("user_input", forKey: "type")
            try container.encode(text, forKey: "text")
        case .activeInput(let operation, let turnID, let text):
            try container.encode("active_input", forKey: "type")
            try container.encode(operation, forKey: "operation")
            try container.encode(turnID, forKey: "turnId")
            try container.encode(text, forKey: "text")
        case .interrupt(let turnID):
            try container.encode("interrupt", forKey: "type")
            try container.encode(turnID, forKey: "turnId")
        case .execApproval(let id, let decision):
            try container.encode("exec_approval", forKey: "type")
            try container.encode(id, forKey: "id")
            try container.encode(decision, forKey: "decision")
        case .capabilityCommand(let capability, let command, let arguments):
            try container.encode("capability_command", forKey: "type")
            try container.encode(capability, forKey: "capability")
            try container.encode(command, forKey: "command")
            try container.encode(arguments, forKey: "arguments")
        case .setModel(let route):
            try container.encode("set_model", forKey: "type")
            try container.encode(route, forKey: "route")
        case .resumeSession(let sessionID):
            try container.encode("resume_session", forKey: "type")
            try container.encode(sessionID, forKey: "sessionId")
        case .unknown:
            break
        }
    }
}

enum ReviewDecision: Codable, Sendable {
    case approved
    case approvedForSession
    case denied(rejection: String)
    case abort

    init(from decoder: Decoder) throws {
        self = try Self(json: JSONValue(from: decoder))
    }

    init(json: JSONValue) throws {
        if let value = json.stringValue {
            switch value {
            case "approved": self = .approved
            case "approved_for_session": self = .approvedForSession
            case "abort": self = .abort
            default: throw GatewayWireError.invalidFrame("unknown review decision \(value)")
            }
            return
        }
        if let rejection = json["denied"]?["rejection"]?.stringValue {
            self = .denied(rejection: rejection)
            return
        }
        throw GatewayWireError.invalidFrame("invalid review decision")
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .approved:
            var container = encoder.singleValueContainer()
            try container.encode("approved")
        case .approvedForSession:
            var container = encoder.singleValueContainer()
            try container.encode("approved_for_session")
        case .denied(let rejection):
            var container = encoder.container(keyedBy: DynamicCodingKey.self)
            try container.encode(["rejection": rejection], forKey: "denied")
        case .abort:
            var container = encoder.singleValueContainer()
            try container.encode("abort")
        }
    }
}

enum GatewayRequest: Encodable, Sendable {
    case pair(code: String, clientLabel: String, lastSequence: UInt64?)
    case authenticate(token: String, lastSequence: UInt64?)
    case openSession(requestID: String, sessionID: String?)
    case renameSession(requestID: String, sessionID: String, title: String)
    case setSessionPinned(requestID: String, sessionID: String, pinned: Bool)
    case deleteSession(requestID: String, sessionID: String)
    case submit(Submission)
    case configureAgent(requestID: String, expectedRevision: UInt64, config: AgentComposition)
    case setWorkspace(requestID: String, path: String)
    case setGitBranch(requestID: String, branch: String)
    case listDirectories(requestID: String, path: String, includeFiles: Bool)
    case setProviderCredential(requestID: String, provider: String, apiKey: String)
    case createPairingCode(requestID: String)
    case startProviderLogin(requestID: String, provider: String)
    case getProfile(requestID: String)
    case listArtifacts(requestID: String)
    case getGitDiff(requestID: String)
    case addCron(requestID: String, task: String, schedule: String)
    case listCron(requestID: String)
    case rescheduleCron(requestID: String, id: String, schedule: String)
    case deleteCron(requestID: String, id: String)
    case runCron(requestID: String, id: String)
    case listCronHistory(requestID: String, id: String?)

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: DynamicCodingKey.self)
        try container.encode(gatewayProtocolVersion, forKey: "version")
        switch self {
        case .pair(let code, let clientLabel, let lastSequence):
            try container.encode("pair", forKey: "type")
            try container.encode(code, forKey: "code")
            try container.encode(clientLabel, forKey: "clientLabel")
            try container.encodeIfPresent(lastSequence, forKey: "lastSequence")
        case .authenticate(let token, let lastSequence):
            try container.encode("authenticate", forKey: "type")
            try container.encode(token, forKey: "token")
            try container.encodeIfPresent(lastSequence, forKey: "lastSequence")
        case .openSession(let requestID, let sessionID):
            try container.encode("open_session", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encodeIfPresent(sessionID, forKey: "sessionId")
        case .renameSession(let requestID, let sessionID, let title):
            try container.encode("rename_session", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(title, forKey: "title")
        case .setSessionPinned(let requestID, let sessionID, let pinned):
            try container.encode("set_session_pinned", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(pinned, forKey: "pinned")
        case .deleteSession(let requestID, let sessionID):
            try container.encode("delete_session", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
        case .submit(let submission):
            try container.encode("submit", forKey: "type")
            try container.encode(submission, forKey: "submission")
        case .configureAgent(let requestID, let expectedRevision, let config):
            try container.encode("configure_agent", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(expectedRevision, forKey: "expectedRevision")
            try container.encode(config, forKey: "config")
        case .setWorkspace(let requestID, let path):
            try container.encode("set_workspace", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(path, forKey: "path")
        case .setGitBranch(let requestID, let branch):
            try container.encode("set_git_branch", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(branch, forKey: "branch")
        case .listDirectories(let requestID, let path, let includeFiles):
            try container.encode("list_directories", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(path, forKey: "path")
            try container.encode(includeFiles, forKey: "includeFiles")
        case .setProviderCredential(let requestID, let provider, let apiKey):
            try container.encode("set_provider_credential", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(provider, forKey: "provider")
            try container.encode(apiKey, forKey: "apiKey")
        case .createPairingCode(let requestID):
            try container.encode("create_pairing_code", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
        case .startProviderLogin(let requestID, let provider):
            try container.encode("start_provider_login", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(provider, forKey: "provider")
        case .getProfile(let requestID):
            try container.encode("get_profile", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
        case .listArtifacts(let requestID):
            try container.encode("list_artifacts", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
        case .getGitDiff(let requestID):
            try container.encode("get_git_diff", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
        case .addCron(let requestID, let task, let schedule):
            try container.encode("add_cron", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(task, forKey: "task")
            try container.encode(schedule, forKey: "schedule")
        case .listCron(let requestID):
            try container.encode("list_cron", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
        case .rescheduleCron(let requestID, let id, let schedule):
            try container.encode("reschedule_cron", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(id, forKey: "id")
            try container.encode(schedule, forKey: "schedule")
        case .deleteCron(let requestID, let id):
            try container.encode("delete_cron", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(id, forKey: "id")
        case .runCron(let requestID, let id):
            try container.encode("run_cron", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(id, forKey: "id")
        case .listCronHistory(let requestID, let id):
            try container.encode("list_cron_history", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encodeIfPresent(id, forKey: "id")
        }
    }
}

enum GatewayEnvelope: Decodable, Sendable {
    case paired(clientID: String, token: String)
    case authenticated
    case ready(ReadyPayload)
    case accepted(requestID: String)
    case rejected(GatewayRejection)
    case agentEvent(
        sequence: UInt64,
        event: AgentEventRecord,
        blocks: [FrontendBlock],
        history: [RenderedEventRecord]?,
        preview: RenderedPreview?
    )
    case sessions([SessionSummary])
    case configChanged(VersionedAgentConfig)
    case providerCredentialStatus(requestID: String, provider: String, configured: Bool)
    case pairingCode(requestID: String, code: String, expiresAt: Int64)
    case providerLoginStarted(
        requestID: String,
        loginID: String,
        provider: String,
        verificationURL: String,
        userCode: String
    )
    case providerLoginFinished(requestID: String, loginID: String, provider: String)
    case profile(requestID: String, profile: ProfileSnapshot)
    case artifacts(requestID: String, artifacts: [ArtifactRecord])
    case gitDiff(requestID: String, diff: String)
    case directories(requestID: String, listing: DirectoryListing)
    case cronTasks(requestID: String, tasks: [CronTask])
    case cronHistory(requestID: String, runs: [CronRun])
    case error(GatewayFailure)
    case unknown(type: String)

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: DynamicCodingKey.self)
        let version = try container.decode(Int.self, forKey: "version")
        guard version == gatewayProtocolVersion else {
            throw GatewayWireError.unsupportedVersion(version)
        }
        let type = try container.decode(String.self, forKey: "type")
        switch type {
        case "paired":
            self = .paired(
                clientID: try container.decode(String.self, forKey: "clientId"),
                token: try container.decode(String.self, forKey: "token")
            )
        case "authenticated":
            self = .authenticated
        case "ready":
            self = .ready(try container.decode(ReadyPayload.self, forKey: "payload"))
        case "accepted":
            self = .accepted(requestID: try container.decode(String.self, forKey: "requestId"))
        case "rejected":
            self = .rejected(try GatewayRejection(from: decoder))
        case "agent_event":
            self = .agentEvent(
                sequence: try container.decode(UInt64.self, forKey: "sequence"),
                event: try container.decode(AgentEventRecord.self, forKey: "event"),
                blocks: try container.decode([FrontendBlock].self, forKey: "blocks"),
                history: try container.decodeIfPresent([RenderedEventRecord].self, forKey: "history"),
                preview: try container.decodeIfPresent(RenderedPreview.self, forKey: "preview")
            )
        case "sessions":
            self = .sessions(try container.decode([SessionSummary].self, forKey: "sessions"))
        case "config_changed":
            self = .configChanged(try container.decode(VersionedAgentConfig.self, forKey: "snapshot"))
        case "provider_credential_status":
            self = .providerCredentialStatus(
                requestID: try container.decode(String.self, forKey: "requestId"),
                provider: try container.decode(String.self, forKey: "provider"),
                configured: try container.decode(Bool.self, forKey: "configured")
            )
        case "pairing_code":
            self = .pairingCode(
                requestID: try container.decode(String.self, forKey: "requestId"),
                code: try container.decode(String.self, forKey: "code"),
                expiresAt: try container.decode(Int64.self, forKey: "expiresAt")
            )
        case "provider_login_started":
            self = .providerLoginStarted(
                requestID: try container.decode(String.self, forKey: "requestId"),
                loginID: try container.decode(String.self, forKey: "loginId"),
                provider: try container.decode(String.self, forKey: "provider"),
                verificationURL: try container.decode(String.self, forKey: "verificationUrl"),
                userCode: try container.decode(String.self, forKey: "userCode")
            )
        case "provider_login_finished":
            self = .providerLoginFinished(
                requestID: try container.decode(String.self, forKey: "requestId"),
                loginID: try container.decode(String.self, forKey: "loginId"),
                provider: try container.decode(String.self, forKey: "provider")
            )
        case "profile":
            self = .profile(
                requestID: try container.decode(String.self, forKey: "requestId"),
                profile: try container.decode(ProfileSnapshot.self, forKey: "profile")
            )
        case "artifacts":
            self = .artifacts(
                requestID: try container.decode(String.self, forKey: "requestId"),
                artifacts: try container.decode([ArtifactRecord].self, forKey: "artifacts")
            )
        case "git_diff":
            self = .gitDiff(
                requestID: try container.decode(String.self, forKey: "requestId"),
                diff: try container.decode(String.self, forKey: "diff")
            )
        case "directories":
            self = .directories(
                requestID: try container.decode(String.self, forKey: "requestId"),
                listing: try container.decode(DirectoryListing.self, forKey: "listing")
            )
        case "cron_tasks":
            self = .cronTasks(
                requestID: try container.decode(String.self, forKey: "requestId"),
                tasks: try container.decode([CronTask].self, forKey: "tasks")
            )
        case "cron_history":
            self = .cronHistory(
                requestID: try container.decode(String.self, forKey: "requestId"),
                runs: try container.decode([CronRun].self, forKey: "runs")
            )
        case "error":
            self = .error(try GatewayFailure(from: decoder))
        default:
            self = .unknown(type: type)
        }
    }
}

struct GatewayRejection: Decodable, Sendable {
    let requestId: String
    let code: String
    let message: String
    let fatal: Bool
}

struct GatewayFailure: Decodable, Sendable {
    let code: String
    let message: String
    let fatal: Bool
}

struct ReadyPayload: Decodable, Sendable {
    let latestSequence: UInt64
    let workspace: WorkspaceSummary
    let session: SessionConfigured
    let sessions: [SessionSummary]
    let modelChoices: [ModelChoice]
    let contributions: [FrontendContribution]
    let config: VersionedAgentConfig
    let providers: [ProviderStatus]
    let git: GitStatus?
}

struct WorkspaceSummary: Identifiable, Codable, Hashable, Sendable {
    let id: String
    let label: String
}

struct GitStatus: Codable, Equatable, Sendable {
    let currentBranch: String
    let branches: [String]
}

struct DirectoryListing: Codable, Equatable, Sendable {
    let path: String
    let parent: String?
    let entries: [DirectoryEntry]
}

struct DirectoryEntry: Identifiable, Codable, Equatable, Sendable {
    var id: String { path }

    let name: String
    let path: String
    let isDirectory: Bool
}

struct SessionConfigured: Decodable, Sendable {
    let sessionId: String
    let context: SessionContext
    let model: ModelChanged
}

struct SessionContext: Codable, Hashable, Sendable {
    var tenantId: String?
    var userId: String?
    var userName: String?
    var workspaceId: String?
    var workspaceLabel: String?
    var originLabel: String?
}

struct ModelChanged: Codable, Hashable, Sendable {
    let route: String
    let model: String
    let reasoningEffort: String?
    let modelContextWindow: Int64?
}

struct SessionSummary: Identifiable, Codable, Hashable, Sendable {
    var id: String { sessionId }

    let sessionId: String
    let sessionContext: SessionContext
    let parentSessionId: String?
    let parentSequence: UInt64?
    let sequence: UInt64
    let catalogVisible: Bool
    let firstUserMessage: String?
    let title: String?
    let pinned: Bool
    let createdAt: Int64
    let updatedAt: Int64
}

struct ModelChoice: Identifiable, Codable, Hashable, Sendable {
    var id: String { route }

    let route: String
    let group: String
    let model: String
    let reasoningEffort: String?
    let contextWindow: Int64?
}

struct FrontendContribution: Codable, Sendable {
    let capability: String
    let commands: [FrontendCommand]
    let widgets: [FrontendWidget]
    let references: [FrontendReference]
    let activeInput: FrontendActiveInput?
}

struct FrontendCommand: Codable, Hashable, Sendable {
    let name: String
    let arguments: String
    let description: String
}

struct FrontendWidget: Identifiable, Codable, Sendable {
    let id: String
    let slot: String
    let text: String
    let tone: String
    let action: AgentOperation?
}

extension FrontendWidget {
    init(json: JSONValue) throws {
        guard let id = json["id"]?.stringValue,
              let slot = json["slot"]?.stringValue,
              let text = json["text"]?.stringValue,
              let tone = json["tone"]?.stringValue
        else {
            throw GatewayWireError.invalidFrame("frontend widget is missing a required field")
        }
        self.id = id
        self.slot = slot
        self.text = text
        self.tone = tone
        self.action = try json["action"].map(AgentOperation.init(json:))
    }
}

struct FrontendReference: Codable, Hashable, Sendable {
    let trigger: Character
    let value: String
    let description: String

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let encodedTrigger = try container.decode(String.self, forKey: .trigger)
        guard encodedTrigger.count == 1, let trigger = encodedTrigger.first else {
            throw GatewayWireError.invalidFrame("frontend reference trigger must be one character")
        }
        self.trigger = trigger
        self.value = try container.decode(String.self, forKey: .value)
        self.description = try container.decode(String.self, forKey: .description)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(String(trigger), forKey: .trigger)
        try container.encode(value, forKey: .value)
        try container.encode(description, forKey: .description)
    }

    private enum CodingKeys: String, CodingKey {
        case trigger
        case value
        case description
    }
}

struct FrontendActiveInput: Codable, Hashable, Sendable {
    let operation: String
}

struct FrontendBlock: Codable, Hashable, Sendable {
    let id: String?
    let group: String?
    let append: Bool
    let pending: Bool
    let text: String
    let format: String
    let tone: String
}

struct RenderedPreview: Decodable, Sendable {
    let title: String
    let events: [RenderedEventRecord]
}

struct RenderedEventRecord: Decodable, Sendable {
    let event: JSONValue
    let blocks: [FrontendBlock]

    var previewText: [String] {
        if !blocks.isEmpty { return blocks.map(\.text) }
        let type = event["type"]?.stringValue
        let value = type == "agent_reasoning_content_delta"
            ? event["delta"]?.stringValue
            : event["message"]?.stringValue
        guard let value, !value.isEmpty else { return [] }
        let label = switch type {
        case "user_message": "User"
        case "agent_message": "Horus"
        case "agent_reasoning_content_delta": "Working notes"
        case "error": "Error"
        default: "Event"
        }
        return ["\(label)\n\(value)"]
    }
}

struct FrontendPickerOption: Identifiable, Sendable {
    let id = UUID()
    let label: String
    let description: String
    let op: AgentOperation

    init(json: JSONValue) throws {
        guard let label = json["label"]?.stringValue,
              let description = json["description"]?.stringValue,
              let op = json["op"]
        else {
            throw GatewayWireError.invalidFrame("frontend picker option is missing a required field")
        }
        self.label = label
        self.description = description
        self.op = try AgentOperation(json: op)
    }
}

struct AgentEventRecord: Decodable, Sendable {
    let submissionId: String?
    let msg: JSONValue
}

struct VersionedAgentConfig: Codable, Equatable, Sendable {
    let revision: UInt64
    let config: AgentComposition
}

func refreshedAgentDraft(
    currentDraft: AgentComposition?,
    currentSnapshot: VersionedAgentConfig?,
    incomingSnapshot: VersionedAgentConfig
) -> AgentComposition {
    guard currentSnapshot?.revision == incomingSnapshot.revision, let currentDraft else {
        return incomingSnapshot.config
    }
    return currentDraft
}

struct AgentComposition: Codable, Equatable, Sendable {
    var provider: ProviderSelection
    var middleware: MiddlewareSelection
    var approval: ApprovalPolicy
    var systemPrompt: String
}

enum ApprovalPolicy: String, Codable, CaseIterable, Identifiable, Sendable {
    case on
    case allow
    case allowNetwork = "allow_network"

    var id: Self { self }
}

struct ProviderSelection: Codable, Equatable, Sendable {
    var provider: String
    var model: String
    var baseUrl: String?
    var apiKeyEnv: String?
    var reasoningEffort: String?
    var webSearch: String
}

struct MiddlewareSelection: Codable, Equatable, Sendable {
    var tools: Bool
    var skills: Bool
    var subagents: Bool
    var steering: Bool
    var compaction: Bool
    var sessions: Bool
}

struct ProviderStatus: Identifiable, Codable, Equatable, Sendable {
    var id: String { provider }

    let provider: String
    let label: String
    var configured: Bool
    let auth: String
    let defaultModel: String?
    let defaultBaseUrl: String?
    let defaultReasoningEffort: String?
    let defaultWebSearch: String
}

struct TokenUsage: Codable, Equatable, Sendable {
    var inputTokens = 0
    var cachedInputTokens = 0
    var cacheWriteInputTokens = 0
    var outputTokens = 0
    var reasoningOutputTokens = 0
    var totalTokens = 0
}

struct ProfileSnapshot: Codable, Equatable, Sendable {
    let userName: String?
    let workspace: WorkspaceSummary
    let dailyUsage: [DailyUsage]
}

struct DailyUsage: Codable, Equatable, Sendable {
    let unixDay: UInt64
    let usage: TokenUsage
}

struct ArtifactRecord: Identifiable, Codable, Equatable, Sendable {
    let id: String
    let sessionId: String
    let kind: String
    let title: String
    let block: FrontendBlock
}

struct CronTask: Identifiable, Codable, Equatable, Sendable {
    let id: String
    let task: String
    let schedule: String
}

struct CronRun: Identifiable, Codable, Equatable, Sendable {
    let id: String
    let taskId: String
    let startedAt: Int64
    let finishedAt: Int64?
    let status: String
    let sessionId: String?
    let message: String?
}

indirect enum JSONValue: Codable, Equatable, Sendable {
    case object([String: JSONValue])
    case array([JSONValue])
    case string(String)
    case number(Double)
    case bool(Bool)
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() { self = .null }
        else if let value = try? container.decode(Bool.self) { self = .bool(value) }
        else if let value = try? container.decode(Double.self) { self = .number(value) }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? container.decode([JSONValue].self) { self = .array(value) }
        else {
            let keyed = try decoder.container(keyedBy: DynamicCodingKey.self)
            self = .object(try Dictionary(uniqueKeysWithValues: keyed.allKeys.map { key in
                (key.stringValue, try keyed.decode(JSONValue.self, forKey: key))
            }))
        }
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .object(let value):
            var container = encoder.container(keyedBy: DynamicCodingKey.self)
            for (key, element) in value {
                try container.encode(element, forKey: DynamicCodingKey(key))
            }
        case .array(let value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case .string(let value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case .number(let value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case .bool(let value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case .null:
            var container = encoder.singleValueContainer()
            try container.encodeNil()
        }
    }

    subscript(key: String) -> JSONValue? {
        guard case .object(let object) = self else { return nil }
        return object[key]
    }

    var stringValue: String? {
        guard case .string(let value) = self else { return nil }
        return value
    }

    var intValue: Int? {
        guard case .number(let value) = self, value.rounded() == value else { return nil }
        return Int(exactly: value)
    }

    var boolValue: Bool? {
        guard case .bool(let value) = self else { return nil }
        return value
    }

    var arrayValue: [JSONValue]? {
        guard case .array(let value) = self else { return nil }
        return value
    }

    var objectValue: [String: JSONValue]? {
        guard case .object(let value) = self else { return nil }
        return value
    }
}

struct DynamicCodingKey: CodingKey, Hashable {
    let stringValue: String
    let intValue: Int?

    init(stringValue: String) {
        self.stringValue = stringValue
        self.intValue = nil
    }

    init(intValue: Int) {
        self.stringValue = String(intValue)
        self.intValue = intValue
    }

    init(_ string: String) {
        self.init(stringValue: string)
    }
}

extension KeyedEncodingContainer where Key == DynamicCodingKey {
    mutating func encode<T: Encodable>(_ value: T, forKey key: String) throws {
        try encode(value, forKey: DynamicCodingKey(key))
    }

    mutating func encodeIfPresent<T: Encodable>(_ value: T?, forKey key: String) throws {
        try encodeIfPresent(value, forKey: DynamicCodingKey(key))
    }
}

extension KeyedDecodingContainer where Key == DynamicCodingKey {
    func decode<T: Decodable>(_ type: T.Type, forKey key: String) throws -> T {
        try decode(type, forKey: DynamicCodingKey(key))
    }

    func decodeIfPresent<T: Decodable>(_ type: T.Type, forKey key: String) throws -> T? {
        try decodeIfPresent(type, forKey: DynamicCodingKey(key))
    }
}
