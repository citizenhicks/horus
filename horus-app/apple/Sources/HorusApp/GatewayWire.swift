import Foundation
#if os(iOS)
import UIKit
#endif

let gatewayProtocolVersion = 19
let maximumGatewayFrameBytes = 2 * 1024 * 1024
let maximumComposerBytes = 1024 * 1024
let maximumAttachmentReferences = 16

enum GatewayWireError: LocalizedError, Equatable {
    case invalidEndpoint(String)
    case invalidPairingSetup
    case insecureRemoteEndpoint
    case unsupportedVersion(Int)
    case oversizedFrame(Int)
    case invalidFrame(String)
    case disconnected

    var errorDescription: String? {
        switch self {
        case .invalidEndpoint(let message): message
        case .invalidPairingSetup:
            "Use a complete Horus pairing setup from the gateway."
        case .insecureRemoteEndpoint:
            "Plaintext gateway connections are allowed only on this device. Use tls:// or wss:// for remote gateways."
        case .unsupportedVersion(let version): "Gateway protocol version \(version) is not supported."
        case .oversizedFrame(let size): "Gateway frame is too large (\(size) bytes)."
        case .invalidFrame(let message): "Invalid gateway frame: \(message)"
        case .disconnected: "The gateway disconnected."
        }
    }
}

struct GatewayPairingSetup: Equatable, Sendable {
    private static let maximumCodeBytes = 512

    let endpoint: GatewayEndpoint
    let code: String

    init(_ rawValue: String) throws {
        let parts = rawValue.split(separator: "|", omittingEmptySubsequences: false)
        guard parts.count == 3, parts[0] == "horus-pair:v1" else {
            throw GatewayWireError.invalidPairingSetup
        }
        try self.init(endpoint: String(parts[1]), code: String(parts[2]))
    }

    init(url: URL) throws {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.scheme?.lowercased() == "horus",
              components.host?.lowercased() == "pair",
              components.user == nil,
              components.password == nil,
              components.port == nil,
              components.path.isEmpty || components.path == "/",
              components.fragment == nil,
              let queryItems = components.queryItems,
              queryItems.count == 2,
              Set(queryItems.map(\.name)) == ["endpoint", "code"],
              let endpoint = queryItems.first(where: { $0.name == "endpoint" })?.value,
              let code = queryItems.first(where: { $0.name == "code" })?.value
        else {
            throw GatewayWireError.invalidPairingSetup
        }
        try self.init(endpoint: endpoint, code: code)
    }

    private init(endpoint: String, code: String) throws {
        guard !code.isEmpty,
              code.utf8.count <= Self.maximumCodeBytes,
              code.utf8.allSatisfy({ $0 >= 0x21 && $0 <= 0x7e })
        else {
            throw GatewayWireError.invalidPairingSetup
        }
        self.endpoint = try GatewayEndpoint(endpoint)
        self.code = code
    }
}

struct GatewayEndpoint: Hashable, Codable, Sendable {
    let rawValue: String

    init(_ rawValue: String) throws {
        let trimmed = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let components = URLComponents(string: trimmed),
              let scheme = components.scheme?.lowercased(),
              let parsedHost = components.host,
              components.user == nil,
              components.password == nil,
              components.query == nil,
              components.fragment == nil,
              components.path.isEmpty || components.path == "/"
        else {
            throw GatewayWireError.invalidEndpoint(
                "Use tcp://host:port, tls://host:port, or wss://host."
            )
        }
        guard scheme == "tcp" || scheme == "tls" || scheme == "wss" else {
            throw GatewayWireError.invalidEndpoint(
                "The endpoint scheme must be tcp://, tls://, or wss://."
            )
        }
        guard let port = components.port ?? (scheme == "wss" ? 443 : nil),
              (1...65_535).contains(port)
        else {
            throw GatewayWireError.invalidEndpoint(
                "Use tcp://host:port, tls://host:port, or wss://host."
            )
        }
        let host = Self.normalized(host: parsedHost)
        guard !host.isEmpty else {
            throw GatewayWireError.invalidEndpoint(
                "Use tcp://host:port, tls://host:port, or wss://host."
            )
        }
        if scheme == "tcp" && !Self.isLoopback(host) {
            throw GatewayWireError.insecureRemoteEndpoint
        }
        let suffix = scheme == "wss" && port == 443 ? "" : ":\(port)"
        self.rawValue = "\(scheme)://\(Self.formatted(host: host))\(suffix)"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        try self.init(container.decode(String.self, forKey: .rawValue))
    }

    var usesTLS: Bool { rawValue.hasPrefix("tls://") || usesWebSocket }

    var usesWebSocket: Bool { rawValue.hasPrefix("wss://") }

    var host: String {
        Self.normalized(host: URLComponents(string: rawValue)?.host ?? "")
    }

    var port: UInt16 {
        UInt16(URLComponents(string: rawValue)?.port ?? (usesWebSocket ? 443 : 0))
    }

    var displayName: String {
        if Self.isLoopback(host) {
            #if os(macOS)
            return "This Mac · \(port)"
            #else
            return "This device · \(port)"
            #endif
        }
        let quickSuffix = ".trycloudflare.com"
        if host.hasSuffix(quickSuffix) {
            let words = host.dropLast(quickSuffix.count).split(separator: "-")
            let tunnel = words.count > 1
                ? "\(words[0])…\(words[words.count - 1])"
                : String(words.first ?? "Tunnel")
            return "Cloudflare · \(tunnel)"
        }
        return port == 443 ? host : "\(host):\(port)"
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

struct AttachmentRecord: Identifiable, Codable, Hashable, Sendable {
    private enum CodingKeys: String, CodingKey { case id, name, size, mediaType }

    let id: String
    let name: String
    let size: Int64
    let mediaType: String

    init(id: String, name: String, size: Int64, mediaType: String) {
        self.id = id
        self.name = name
        self.size = size
        self.mediaType = mediaType
    }

    init(json: JSONValue) throws {
        guard let id = json["id"]?.stringValue,
              !id.isEmpty,
              let name = json["name"]?.stringValue,
              !name.isEmpty,
              let size = json["size"]?.intValue,
              size >= 0,
              let mediaType = json["mediaType"]?.stringValue,
              !mediaType.isEmpty
        else {
            throw GatewayWireError.invalidFrame("attachment is missing a required field")
        }
        self.init(id: id, name: name, size: Int64(size), mediaType: mediaType)
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let id = try container.decode(String.self, forKey: .id)
        let name = try container.decode(String.self, forKey: .name)
        let size = try container.decode(Int64.self, forKey: .size)
        let mediaType = try container.decode(String.self, forKey: .mediaType)
        guard !id.isEmpty, !name.isEmpty, size >= 0, !mediaType.isEmpty else {
            throw GatewayWireError.invalidFrame("attachment is missing a required field")
        }
        self.init(id: id, name: name, size: size, mediaType: mediaType)
    }
}

struct MessageTarget: Codable, Hashable, Sendable {
    let checkpointSequence: UInt64
    let batchItemCount: Int

    init(checkpointSequence: UInt64, batchItemCount: Int) {
        self.checkpointSequence = checkpointSequence
        self.batchItemCount = batchItemCount
    }

    init?(json: JSONValue) {
        guard let sequenceValue = json["checkpointSequence"],
              case .number(let sequence) = sequenceValue,
              let checkpointSequence = UInt64(exactly: sequence),
              let countValue = json["batchItemCount"],
              case .number(let count) = countValue,
              let batchItemCount = Int(exactly: count),
              batchItemCount > 0
        else { return nil }
        self.init(checkpointSequence: checkpointSequence, batchItemCount: batchItemCount)
    }
}

enum AgentOperation: Codable, Sendable {
    case userInput(text: String, attachments: [AttachmentRecord])
    case activeInput(operation: String, turnID: String, text: String)
    case interrupt(turnID: String)
    case execApproval(id: String, decision: ReviewDecision)
    case capabilityCommand(
        capability: String,
        command: String,
        arguments: String,
        input: String?,
        target: MessageTarget?
    )
    case setModel(route: String)
    case resumeSession(sessionID: String)

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
            guard let values = value["attachments"]?.arrayValue,
                  values.count <= maximumAttachmentReferences
            else {
                throw GatewayWireError.invalidFrame("user_input has invalid attachments")
            }
            self = .userInput(
                text: try required("text"),
                attachments: try values.map(AttachmentRecord.init(json:))
            )
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
            guard let inputValue = value["input"], let targetValue = value["target"] else {
                throw GatewayWireError.invalidFrame("capability_command has no input or target")
            }
            let input: String?
            switch inputValue {
            case .string(let value): input = value
            case .null: input = nil
            default: throw GatewayWireError.invalidFrame("capability_command has invalid input")
            }
            let target: MessageTarget?
            if targetValue != .null {
                guard let decoded = MessageTarget(json: targetValue) else {
                    throw GatewayWireError.invalidFrame("capability_command has invalid target")
                }
                target = decoded
            } else {
                target = nil
            }
            self = .capabilityCommand(
                capability: try required("capability"),
                command: try required("command"),
                arguments: try required("arguments"),
                input: input,
                target: target
            )
        case "set_model":
            self = .setModel(route: try required("route"))
        case "resume_session":
            self = .resumeSession(sessionID: try required("sessionId"))
        default:
            throw GatewayWireError.invalidFrame("unknown agent operation \(type)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: DynamicCodingKey.self)
        switch self {
        case .userInput(let text, let attachments):
            guard attachments.count <= maximumAttachmentReferences else {
                throw GatewayWireError.invalidFrame("user_input has too many attachments")
            }
            try container.encode("user_input", forKey: "type")
            try container.encode(text, forKey: "text")
            try container.encode(attachments, forKey: "attachments")
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
        case .capabilityCommand(let capability, let command, let arguments, let input, let target):
            try container.encode("capability_command", forKey: "type")
            try container.encode(capability, forKey: "capability")
            try container.encode(command, forKey: "command")
            try container.encode(arguments, forKey: "arguments")
            try container.encode(input, forKey: "input")
            try container.encode(target, forKey: "target")
        case .setModel(let route):
            try container.encode("set_model", forKey: "type")
            try container.encode(route, forKey: "route")
        case .resumeSession(let sessionID):
            try container.encode("resume_session", forKey: "type")
            try container.encode(sessionID, forKey: "sessionId")
        }
    }
}

extension AgentOperation {
    var capabilityInput: String? {
        guard case .capabilityCommand(_, _, _, let input, _) = self else { return nil }
        return input
    }

    func replacingCapabilityInput(with input: String) -> Self {
        guard case .capabilityCommand(
            let capability,
            let command,
            let arguments,
            _,
            let target
        ) = self else { return self }
        return .capabilityCommand(
            capability: capability,
            command: command,
            arguments: arguments,
            input: input,
            target: target
        )
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
    case pair(code: String, clientLabel: String, clientKind: GatewayClientKind)
    case authenticate(token: String, clientKind: GatewayClientKind)
    case listClients(requestID: String)
    case unpairClient(requestID: String, clientID: String)
    case listSessions(requestID: String)
    case createSession(requestID: String, workspace: String)
    case openSession(
        requestID: String,
        sessionID: String,
        lastSequence: UInt64?,
        replayEpoch: String?
    )
    case getSessionHistory(
        requestID: String,
        sessionID: String,
        beforeSequence: UInt64?,
        maxBatches: Int
    )
    case renameSession(requestID: String, sessionID: String, title: String)
    case setSessionPinned(requestID: String, sessionID: String, pinned: Bool)
    case deleteSession(requestID: String, sessionID: String)
    case submit(sessionID: String, submission: Submission)
    case configureSession(
        requestID: String,
        sessionID: String,
        expectedRevision: UInt64,
        config: AgentComposition
    )
    case configureDefaultAgent(
        requestID: String,
        expectedRevision: UInt64,
        config: AgentComposition
    )
    case getGitDiff(requestID: String, sessionID: String, scope: GitDiffScope)
    case listWorkspaceFiles(requestID: String, sessionID: String, scope: WorkspaceFileScope)
    case readWorkspaceFile(
        requestID: String,
        sessionID: String,
        path: String,
        offset: UInt64,
        maxBytes: Int
    )
    case beginAttachmentUpload(
        requestID: String,
        sessionID: String,
        name: String,
        size: Int64,
        mediaType: String
    )
    case appendAttachmentChunk(
        requestID: String,
        sessionID: String,
        uploadID: String,
        offset: Int64,
        data: Data
    )
    case finishAttachmentUpload(requestID: String, sessionID: String, uploadID: String)
    case listAttachments(requestID: String, sessionID: String)
    case readAttachment(
        requestID: String,
        sessionID: String,
        attachmentID: String,
        offset: Int64,
        maxBytes: Int
    )
    case switchGitBranch(requestID: String, sessionID: String, branch: String)
    case listDirectories(requestID: String, path: String, includeFiles: Bool)
    case setProviderCredential(requestID: String, provider: String, apiKey: String)
    case setProviderEndpointCredential(
        requestID: String,
        provider: String,
        baseURL: String,
        apiKey: String
    )
    case registerProvider(requestID: String, config: ProviderConfig, modelIds: [String])
    case createPairingCode(requestID: String)
    case startProviderLogin(requestID: String, provider: String)
    case getProfile(requestID: String)
    case startCronSetup(requestID: String, sessionID: String, task: String?)
    case listCron(requestID: String, sessionID: String)
    case rescheduleCron(requestID: String, sessionID: String, id: String, schedule: String)
    case deleteCron(requestID: String, sessionID: String, id: String)
    case runCron(requestID: String, sessionID: String, id: String)
    case listCronHistory(requestID: String, sessionID: String, id: String?)

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: DynamicCodingKey.self)
        try container.encode(gatewayProtocolVersion, forKey: "version")
        switch self {
        case .pair(let code, let clientLabel, let clientKind):
            try container.encode("pair", forKey: "type")
            try container.encode(code, forKey: "code")
            try container.encode(clientLabel, forKey: "clientLabel")
            try container.encode(clientKind, forKey: "clientKind")
        case .authenticate(let token, let clientKind):
            try container.encode("authenticate", forKey: "type")
            try container.encode(token, forKey: "token")
            try container.encode(clientKind, forKey: "clientKind")
        case .listClients(let requestID):
            try container.encode("list_clients", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
        case .unpairClient(let requestID, let clientID):
            try container.encode("unpair_client", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(clientID, forKey: "clientId")
        case .listSessions(let requestID):
            try container.encode("list_sessions", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
        case .createSession(let requestID, let workspace):
            try container.encode("create_session", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(workspace, forKey: "workspace")
        case .openSession(let requestID, let sessionID, let lastSequence, let replayEpoch):
            try container.encode("open_session", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(lastSequence, forKey: "lastSequence")
            try container.encode(replayEpoch, forKey: "replayEpoch")
        case .getSessionHistory(let requestID, let sessionID, let beforeSequence, let maxBatches):
            try container.encode("get_session_history", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(beforeSequence, forKey: "beforeSequence")
            try container.encode(maxBatches, forKey: "maxBatches")
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
        case .submit(let sessionID, let submission):
            try container.encode("submit", forKey: "type")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(submission, forKey: "submission")
        case .configureSession(let requestID, let sessionID, let expectedRevision, let config):
            try container.encode("configure_session", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(expectedRevision, forKey: "expectedRevision")
            try container.encode(config, forKey: "config")
        case .configureDefaultAgent(let requestID, let expectedRevision, let config):
            try container.encode("configure_default_agent", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(expectedRevision, forKey: "expectedRevision")
            try container.encode(config, forKey: "config")
        case .getGitDiff(let requestID, let sessionID, let scope):
            try container.encode("get_git_diff", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(scope, forKey: "scope")
        case .listWorkspaceFiles(let requestID, let sessionID, let scope):
            try container.encode("list_workspace_files", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(scope, forKey: "scope")
        case .readWorkspaceFile(let requestID, let sessionID, let path, let offset, let maxBytes):
            try container.encode("read_workspace_file", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(path, forKey: "path")
            try container.encode(offset, forKey: "offset")
            try container.encode(maxBytes, forKey: "maxBytes")
        case .beginAttachmentUpload(let requestID, let sessionID, let name, let size, let mediaType):
            try container.encode("begin_attachment_upload", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(name, forKey: "name")
            try container.encode(size, forKey: "size")
            try container.encode(mediaType, forKey: "mediaType")
        case .appendAttachmentChunk(let requestID, let sessionID, let uploadID, let offset, let data):
            try container.encode("append_attachment_chunk", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(uploadID, forKey: "uploadId")
            try container.encode(offset, forKey: "offset")
            try container.encode(data, forKey: "data")
        case .finishAttachmentUpload(let requestID, let sessionID, let uploadID):
            try container.encode("finish_attachment_upload", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(uploadID, forKey: "uploadId")
        case .listAttachments(let requestID, let sessionID):
            try container.encode("list_attachments", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
        case .readAttachment(let requestID, let sessionID, let attachmentID, let offset, let maxBytes):
            try container.encode("read_attachment", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(attachmentID, forKey: "attachmentId")
            try container.encode(offset, forKey: "offset")
            try container.encode(maxBytes, forKey: "maxBytes")
        case .switchGitBranch(let requestID, let sessionID, let branch):
            try container.encode("switch_git_branch", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
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
        case .setProviderEndpointCredential(let requestID, let provider, let baseURL, let apiKey):
            try container.encode("set_provider_endpoint_credential", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(provider, forKey: "provider")
            try container.encode(baseURL, forKey: "baseUrl")
            try container.encode(apiKey, forKey: "apiKey")
        case .registerProvider(let requestID, let config, let modelIds):
            try container.encode("register_provider", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(config, forKey: "config")
            try container.encode(modelIds, forKey: "modelIds")
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
        case .startCronSetup(let requestID, let sessionID, let task):
            try container.encode("start_cron_setup", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(task, forKey: "task")
        case .listCron(let requestID, let sessionID):
            try container.encode("list_cron", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
        case .rescheduleCron(let requestID, let sessionID, let id, let schedule):
            try container.encode("reschedule_cron", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(id, forKey: "id")
            try container.encode(schedule, forKey: "schedule")
        case .deleteCron(let requestID, let sessionID, let id):
            try container.encode("delete_cron", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(id, forKey: "id")
        case .runCron(let requestID, let sessionID, let id):
            try container.encode("run_cron", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(id, forKey: "id")
        case .listCronHistory(let requestID, let sessionID, let id):
            try container.encode("list_cron_history", forKey: "type")
            try container.encode(requestID, forKey: "requestId")
            try container.encode(sessionID, forKey: "sessionId")
            try container.encode(id, forKey: "id")
        }
    }
}

enum GatewayEnvelope: Decodable, Sendable {
    case paired(clientID: String, token: String)
    case authenticated
    case ready(ReadyPayload)
    case sessionOpened(requestID: String, payload: SessionReadyPayload)
    case sessionReplayComplete(requestID: String, sessionID: String)
    case sessionHistory(
        requestID: String,
        sessionID: String,
        events: [RenderedEventRecord],
        nextBeforeSequence: UInt64?
    )
    case sessionChanged(SessionReadyPayload)
    case gatewayConfigured(requestID: String, payload: ReadyPayload)
    case accepted(requestID: String)
    case rejected(GatewayRejection)
    case agentEvent(
        sessionID: String,
        sequence: UInt64,
        event: AgentEventRecord,
        blocks: [FrontendBlock],
        history: [RenderedEventRecord]?,
        preview: RenderedPreview?
    )
    case sessions(requestID: String?, sessions: [SessionRecord])
    case clients(requestID: String, currentClientID: String, clients: [ClientStatus])
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
    case gitDiff(requestID: String, sessionID: String, scope: GitDiffScope, diff: String)
    case workspaceFiles(requestID: String, sessionID: String, files: [WorkspaceFileRecord])
    case workspaceFileChunk(
        requestID: String,
        sessionID: String,
        path: String,
        offset: UInt64,
        data: Data,
        nextOffset: UInt64?
    )
    case attachmentUploadStarted(
        requestID: String,
        sessionID: String,
        uploadID: String,
        maxChunkBytes: Int
    )
    case attachmentChunkAccepted(
        requestID: String,
        sessionID: String,
        uploadID: String,
        nextOffset: Int64
    )
    case attachmentUploaded(
        requestID: String,
        sessionID: String,
        attachment: AttachmentRecord
    )
    case attachments(requestID: String, sessionID: String, attachments: [AttachmentRecord])
    case attachmentChunk(
        requestID: String,
        sessionID: String,
        attachmentID: String,
        offset: Int64,
        data: Data,
        nextOffset: Int64?
    )
    case directories(requestID: String, listing: DirectoryListing)
    case cronTasks(requestID: String, sessionID: String, tasks: [CronTask])
    case cronHistory(requestID: String, sessionID: String, runs: [CronRun])
    case error(GatewayFailure)

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
            self = .ready(try container.decode(
                ReadyPayload.self,
                forKey: "payload"
            ).validated())
        case "session_opened":
            self = .sessionOpened(
                requestID: try container.decode(String.self, forKey: "requestId"),
                payload: try container.decode(SessionReadyPayload.self, forKey: "payload")
            )
        case "session_replay_complete":
            self = .sessionReplayComplete(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId")
            )
        case "session_history":
            self = .sessionHistory(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                events: try container.decode([RenderedEventRecord].self, forKey: "events"),
                nextBeforeSequence: try container.decodeIfPresent(
                    UInt64.self,
                    forKey: "nextBeforeSequence"
                )
            )
        case "session_changed":
            self = .sessionChanged(
                try container.decode(SessionReadyPayload.self, forKey: "payload")
            )
        case "gateway_configured":
            self = .gatewayConfigured(
                requestID: try container.decode(String.self, forKey: "requestId"),
                payload: try container.decode(
                    ReadyPayload.self,
                    forKey: "payload"
                ).validated()
            )
        case "accepted":
            self = .accepted(requestID: try container.decode(String.self, forKey: "requestId"))
        case "rejected":
            self = .rejected(try GatewayRejection(from: decoder))
        case "agent_event":
            self = .agentEvent(
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                sequence: try container.decode(UInt64.self, forKey: "sequence"),
                event: try container.decode(AgentEventRecord.self, forKey: "event"),
                blocks: try container.decode([FrontendBlock].self, forKey: "blocks"),
                history: try container.decodeIfPresent([RenderedEventRecord].self, forKey: "history"),
                preview: try container.decodeIfPresent(RenderedPreview.self, forKey: "preview")
            )
        case "sessions":
            self = .sessions(
                requestID: try container.decodeIfPresent(String.self, forKey: "requestId"),
                sessions: try container.decode([SessionRecord].self, forKey: "sessions")
            )
        case "clients":
            self = .clients(
                requestID: try container.decode(String.self, forKey: "requestId"),
                currentClientID: try container.decode(String.self, forKey: "currentClientId"),
                clients: try container.decode([ClientStatus].self, forKey: "clients")
            )
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
        case "git_diff":
            self = .gitDiff(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                scope: try container.decode(GitDiffScope.self, forKey: "scope"),
                diff: try container.decode(String.self, forKey: "diff")
            )
        case "workspace_files":
            self = .workspaceFiles(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                files: try container.decode([WorkspaceFileRecord].self, forKey: "files")
            )
        case "workspace_file_chunk":
            self = .workspaceFileChunk(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                path: try container.decode(String.self, forKey: "path"),
                offset: try container.decode(UInt64.self, forKey: "offset"),
                data: try container.decode(Data.self, forKey: "data"),
                nextOffset: try container.decodeIfPresent(UInt64.self, forKey: "nextOffset")
            )
        case "attachment_upload_started":
            self = .attachmentUploadStarted(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                uploadID: try container.decode(String.self, forKey: "uploadId"),
                maxChunkBytes: try container.decode(Int.self, forKey: "maxChunkBytes")
            )
        case "attachment_chunk_accepted":
            self = .attachmentChunkAccepted(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                uploadID: try container.decode(String.self, forKey: "uploadId"),
                nextOffset: try container.decode(Int64.self, forKey: "nextOffset")
            )
        case "attachment_uploaded":
            self = .attachmentUploaded(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                attachment: try container.decode(AttachmentRecord.self, forKey: "attachment")
            )
        case "attachments":
            self = .attachments(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                attachments: try container.decode([AttachmentRecord].self, forKey: "attachments")
            )
        case "attachment_chunk":
            self = .attachmentChunk(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                attachmentID: try container.decode(String.self, forKey: "attachmentId"),
                offset: try container.decode(Int64.self, forKey: "offset"),
                data: try container.decode(Data.self, forKey: "data"),
                nextOffset: try container.decodeIfPresent(Int64.self, forKey: "nextOffset")
            )
        case "directories":
            self = .directories(
                requestID: try container.decode(String.self, forKey: "requestId"),
                listing: try container.decode(DirectoryListing.self, forKey: "listing")
            )
        case "cron_tasks":
            self = .cronTasks(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                tasks: try container.decode([CronTask].self, forKey: "tasks")
            )
        case "cron_history":
            self = .cronHistory(
                requestID: try container.decode(String.self, forKey: "requestId"),
                sessionID: try container.decode(String.self, forKey: "sessionId"),
                runs: try container.decode([CronRun].self, forKey: "runs")
            )
        case "error":
            self = .error(try GatewayFailure(from: decoder))
        default:
            throw GatewayWireError.invalidFrame("unknown gateway message \(type)")
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
    let machineName: String
    let sessions: [SessionRecord]
    let providers: [ProviderStatus]
    let defaultConfig: VersionedAgentConfig?
    let models: [ModelChoice]
    let modelProviders: [String: String]
    let middlewareFeatures: [MiddlewareFeature]
    let maxActiveSessions: Int
}

private extension ReadyPayload {
    func validated() throws -> Self {
        guard machineName == machineName.trimmingCharacters(in: .whitespacesAndNewlines),
              !machineName.isEmpty,
              machineName.utf8.count <= 255,
              !machineName.unicodeScalars.contains(where: {
                  CharacterSet.controlCharacters.contains($0)
              })
        else {
            throw GatewayWireError.invalidFrame("gateway machine name is invalid")
        }
        return self
    }
}

struct SessionReadyPayload: Decodable, Sendable {
    let replayEpoch: String
    let latestSequence: UInt64
    let nextBeforeSequence: UInt64?
    let workspace: WorkspaceInfo
    let git: GitStatus?
    let session: SessionConfigured
    let contributions: [FrontendContribution]
    let widgets: [SessionWidget]
    let toolCount: Int
    let runStats: RunStats
    let config: VersionedAgentConfig
}

struct SessionWidget: Decodable, Sendable {
    let capability: String
    let item: FrontendWidget
}

enum GitDiffScope: String, Codable, CaseIterable, Identifiable, Sendable {
    case staged
    case unstaged
    case committed

    var id: Self { self }
}

enum WorkspaceFileScope: String, Codable, CaseIterable, Identifiable, Sendable {
    case modified
    case all

    var id: Self { self }
}

struct WorkspaceFileRecord: Identifiable, Codable, Hashable, Sendable {
    var id: String { path }

    let path: String
    let size: UInt64
}

struct WorkspaceInfo: Identifiable, Codable, Hashable, Sendable {
    let id: String
    let path: String
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

struct SessionRecord: Identifiable, Codable, Hashable, Sendable {
    var id: String { sessionId }

    let sessionId: String
    let sessionContext: SessionContext
    let parentSessionId: String?
    let parentSequence: UInt64?
    let sequence: UInt64
    let catalogVisible: Bool
    let firstUserMessage: String?
    let executionStats: ExecutionStats
    let title: String?
    let pinned: Bool
    let activity: SessionActivity
    let createdAt: Int64
    let updatedAt: Int64
}

struct SessionActivity: Codable, Hashable, Sendable {
    let state: SessionActivityState
    let turnId: String?
    let startedAt: Int64?
    let lastOutcome: SessionOutcome?
    let message: String?
}

enum SessionActivityState: String, Codable, Hashable, Sendable {
    case idle
    case running
    case awaitingApproval = "awaiting_approval"
}

enum SessionOutcome: String, Codable, Hashable, Sendable {
    case completed
    case aborted
    case failed
}

struct ModelChoice: Identifiable, Codable, Hashable, Sendable {
    private enum CodingKeys: String, CodingKey {
        case route, group, model, reasoningEffort, contextWindow, supportsImageInput
    }

    var id: String { route }

    let route: String
    let group: String
    let model: String
    let reasoningEffort: String?
    let contextWindow: Int64?
    let supportsImageInput: Bool

    init(
        route: String,
        group: String,
        model: String,
        reasoningEffort: String?,
        contextWindow: Int64?,
        supportsImageInput: Bool
    ) {
        self.route = route
        self.group = group
        self.model = model
        self.reasoningEffort = reasoningEffort
        self.contextWindow = contextWindow
        self.supportsImageInput = supportsImageInput
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            route: try container.decode(String.self, forKey: .route),
            group: try container.decode(String.self, forKey: .group),
            model: try container.decode(String.self, forKey: .model),
            reasoningEffort: try container.decodeIfPresent(String.self, forKey: .reasoningEffort),
            contextWindow: try container.decodeIfPresent(Int64.self, forKey: .contextWindow),
            supportsImageInput: try container.decode(
                Bool.self,
                forKey: .supportsImageInput
            )
        )
    }
}

struct FrontendContribution: Decodable, Sendable {
    let capability: String
    let acceptsFileAttachments: Bool
    let count: Int?
    let commands: [FrontendCommand]
    let widgets: [FrontendWidget]
    let references: [FrontendReference]
    let activeInput: FrontendActiveInput?
}

extension FrontendContribution {
    private enum CodingKeys: String, CodingKey {
        case capability
        case acceptsFileAttachments
        case count
        case commands
        case widgets
        case references
        case activeInput
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard container.contains(.count) else {
            throw DecodingError.keyNotFound(
                CodingKeys.count,
                .init(
                    codingPath: container.codingPath,
                    debugDescription: "Frontend contribution requires count."
                )
            )
        }
        capability = try container.decode(String.self, forKey: .capability)
        acceptsFileAttachments = try container.decode(
            Bool.self,
            forKey: .acceptsFileAttachments
        )
        count = try container.decodeIfPresent(Int.self, forKey: .count)
        commands = try container.decode([FrontendCommand].self, forKey: .commands)
        widgets = try container.decode([FrontendWidget].self, forKey: .widgets)
        references = try container.decode([FrontendReference].self, forKey: .references)
        activeInput = try container.decodeIfPresent(FrontendActiveInput.self, forKey: .activeInput)
    }
}

struct FrontendCommand: Codable, Hashable, Sendable {
    let name: String
    let arguments: String
    let description: String
}

struct FrontendProgress: Decodable, Sendable {
    let completed: Int
    let total: Int

    var fraction: Double { Double(completed) / Double(total) }
}

struct FrontendContentBlock: Identifiable, Sendable {
    let id = UUID()
    let block: FrontendBlock
}

enum FrontendWidgetContent: Sendable {
    case blocks(title: String, blocks: [FrontendContentBlock])
    case picker(title: String, options: [FrontendPickerOption])
    case actionList(title: String, items: [FrontendActionListItem])

    var title: String {
        switch self {
        case .blocks(let title, _), .picker(let title, _), .actionList(let title, _): title
        }
    }
}

struct FrontendActionListItem: Identifiable, Sendable {
    let id: String
    let text: String
    let state: FrontendListItemState
    let actions: [FrontendActionListAction]

    init(json: JSONValue) throws {
        guard let id = json["id"]?.stringValue,
              !id.isEmpty,
              let text = json["text"]?.stringValue,
              !text.isEmpty,
              let state = json["state"]?.stringValue.flatMap(FrontendListItemState.init(rawValue:)),
              let values = json["actions"]?.arrayValue
        else {
            throw GatewayWireError.invalidFrame("frontend action list item is missing a required field")
        }
        let actions = try values.map(FrontendActionListAction.init(json:))
        guard Set(actions.map(\.id)).count == actions.count else {
            throw GatewayWireError.invalidFrame("frontend action list item has duplicate action IDs")
        }
        self.id = id
        self.text = text
        self.state = state
        self.actions = actions
    }
}

enum FrontendListItemState: String, Equatable, Sendable {
    case plain
    case pending
    case inProgress = "in_progress"
    case completed
}

struct FrontendActionListAction: Identifiable, Sendable {
    let id: String
    let label: String
    let symbol: String
    let tone: String
    let op: AgentOperation

    init(json: JSONValue) throws {
        guard let id = json["id"]?.stringValue,
              !id.isEmpty,
              let label = json["label"]?.stringValue,
              !label.isEmpty,
              let symbol = json["symbol"]?.stringValue,
              !symbol.isEmpty,
              let tone = json["tone"]?.stringValue,
              ["neutral", "success", "warning", "error"].contains(tone),
              let op = json["op"]
        else {
            throw GatewayWireError.invalidFrame("frontend action list action is missing a required field")
        }
        self.id = id
        self.label = label
        self.symbol = symbol
        self.tone = tone
        self.op = try AgentOperation(json: op)
    }
}

enum FrontendSlot: String, Decodable, Equatable, Sendable {
    case header
    case composerHeader = "composer_header"
    case composerFooter = "composer_footer"
    case messageActions = "message_actions"
    case navigation
    case chatMenu = "chat_menu"
}

struct FrontendWidget: Identifiable, Decodable, Sendable {
    let id: String
    let slot: FrontendSlot
    let text: String
    let tone: String
    let symbol: String?
    let iconOnly: Bool
    let progress: FrontendProgress?
    let content: FrontendWidgetContent?
    let action: AgentOperation?
}

extension FrontendWidget {
    init(from decoder: Decoder) throws {
        try self.init(json: JSONValue(from: decoder))
    }

    init(json: JSONValue) throws {
        guard let id = json["id"]?.stringValue,
              let slot = json["slot"]?.stringValue,
              let text = json["text"]?.stringValue,
              let tone = json["tone"]?.stringValue,
              let iconOnly = json["iconOnly"]?.boolValue,
              json["symbol"] != nil,
              json["progress"] != nil,
              json["content"] != nil,
              json["action"] != nil
        else {
            throw GatewayWireError.invalidFrame("frontend widget is missing a required field")
        }
        guard let slot = FrontendSlot(rawValue: slot),
              ["neutral", "success", "warning", "error"].contains(tone)
        else {
            throw GatewayWireError.invalidFrame("frontend widget has an unknown slot or tone")
        }
        self.id = id
        self.slot = slot
        self.text = text
        self.tone = tone
        self.iconOnly = iconOnly
        switch json["symbol"] {
        case .some(.string(let symbol)): self.symbol = symbol
        case .some(.null): self.symbol = nil
        default: throw GatewayWireError.invalidFrame("frontend widget has an invalid symbol")
        }
        switch json["progress"] {
        case .some(.object(let value)):
            guard let completed = value["completed"]?.intValue,
                  let total = value["total"]?.intValue,
                  total > 0,
                  completed >= 0,
                  completed <= total
            else {
                throw GatewayWireError.invalidFrame("frontend widget has invalid progress")
            }
            progress = FrontendProgress(completed: completed, total: total)
        case .some(.null): progress = nil
        default: throw GatewayWireError.invalidFrame("frontend widget has invalid progress")
        }
        switch json["content"] {
        case .some(.object(let value)):
            guard let type = value["type"]?.stringValue,
                  let title = value["title"]?.stringValue
            else {
                throw GatewayWireError.invalidFrame("frontend widget content is missing a required field")
            }
            switch type {
            case "blocks":
                guard let values = value["blocks"]?.arrayValue else {
                    throw GatewayWireError.invalidFrame("frontend widget blocks are missing")
                }
                content = .blocks(
                    title: title,
                    blocks: try values.map { value in
                        FrontendContentBlock(block: try FrontendBlock(json: value))
                    }
                )
            case "picker":
                guard let values = value["options"]?.arrayValue else {
                    throw GatewayWireError.invalidFrame("frontend widget options are missing")
                }
                content = .picker(title: title, options: try values.map(FrontendPickerOption.init(json:)))
            case "action_list":
                guard let values = value["items"]?.arrayValue else {
                    throw GatewayWireError.invalidFrame("frontend widget action list items are missing")
                }
                let items = try values.map(FrontendActionListItem.init(json:))
                guard Set(items.map(\.id)).count == items.count else {
                    throw GatewayWireError.invalidFrame("frontend widget action list has duplicate item IDs")
                }
                content = .actionList(title: title, items: items)
            default:
                throw GatewayWireError.invalidFrame("frontend widget has unknown content")
            }
        case .some(.null): content = nil
        default: throw GatewayWireError.invalidFrame("frontend widget has invalid content")
        }
        if let action = json["action"], action != .null {
            self.action = try AgentOperation(json: action)
        } else {
            self.action = nil
        }
    }
}

struct FrontendReference: Codable, Hashable, Sendable {
    let trigger: Character
    let value: String
    let description: String

    init(trigger: Character, value: String, description: String) {
        self.trigger = trigger
        self.value = value
        self.description = description
    }

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

extension FrontendBlock {
    init(from decoder: Decoder) throws {
        try self.init(json: JSONValue(from: decoder))
    }

    init(json: JSONValue) throws {
        func optionalString(_ key: String) throws -> String? {
            switch json[key] {
            case nil, .some(.null): return nil
            case .some(.string(let value)): return value
            default: throw GatewayWireError.invalidFrame("frontend block has invalid \(key)")
            }
        }

        guard let append = json["append"]?.boolValue,
              let pending = json["pending"]?.boolValue,
              let text = json["text"]?.stringValue,
              let format = json["format"]?.stringValue,
              ["plain_text", "unified_diff"].contains(format),
              let tone = json["tone"]?.stringValue,
              ["neutral", "success", "warning", "error"].contains(tone)
        else {
            throw GatewayWireError.invalidFrame("frontend block is missing a required field")
        }
        id = try optionalString("id")
        group = try optionalString("group")
        self.append = append
        self.pending = pending
        self.text = text
        self.format = format
        self.tone = tone
    }

    func namespaced(to capability: String) -> Self {
        Self(
            id: id.map { "\(capability)/\($0)" },
            group: group.map { "\(capability)/\($0)" },
            append: append,
            pending: pending,
            text: text,
            format: format,
            tone: tone
        )
    }
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

extension RenderedEventRecord {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: DynamicCodingKey.self)
        let event = try container.decode(JSONValue.self, forKey: "event")
        try AgentEventRecord.validate(event)
        self.event = event
        blocks = try container.decode([FrontendBlock].self, forKey: "blocks")
    }
}

struct FrontendPickerOption: Identifiable, Sendable {
    let id = UUID()
    let label: String
    let description: String
    let detail: String
    let op: AgentOperation

    init(json: JSONValue) throws {
        guard let label = json["label"]?.stringValue,
              let description = json["description"]?.stringValue,
              let detail = json["detail"]?.stringValue,
              let op = json["op"]
        else {
            throw GatewayWireError.invalidFrame("frontend picker option is missing a required field")
        }
        self.label = label
        self.description = description
        self.detail = detail
        self.op = try AgentOperation(json: op)
    }
}

struct AgentEventRecord: Decodable, Sendable {
    let submissionId: String?
    let msg: JSONValue
}

extension AgentEventRecord {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: DynamicCodingKey.self)
        let msg = try container.decode(JSONValue.self, forKey: "msg")
        try Self.validate(msg)
        submissionId = try container.decodeIfPresent(String.self, forKey: "submissionId")
        self.msg = msg
    }

    fileprivate static func validate(_ msg: JSONValue) throws {
        guard let type = msg["type"]?.stringValue else {
            throw GatewayWireError.invalidFrame("agent event has no type")
        }

        func requireString(_ key: String, in value: JSONValue = msg) throws {
            guard value[key]?.stringValue != nil else {
                throw GatewayWireError.invalidFrame("\(type) has invalid \(key)")
            }
        }

        func requireBool(_ key: String, in value: JSONValue = msg) throws {
            guard value[key]?.boolValue != nil else {
                throw GatewayWireError.invalidFrame("\(type) has invalid \(key)")
            }
        }

        func requireInteger(_ key: String, in value: JSONValue = msg) throws {
            guard value[key]?.intValue != nil else {
                throw GatewayWireError.invalidFrame("\(type) has invalid \(key)")
            }
        }

        func optionalString(_ key: String, in value: JSONValue = msg) throws {
            guard let field = value[key], field != .null else { return }
            guard field.stringValue != nil else {
                throw GatewayWireError.invalidFrame("\(type) has invalid \(key)")
            }
        }

        func optionalInteger(_ key: String, in value: JSONValue = msg) throws {
            guard let field = value[key], field != .null else { return }
            guard field.intValue != nil else {
                throw GatewayWireError.invalidFrame("\(type) has invalid \(key)")
            }
        }

        func validateContext(_ value: JSONValue) throws {
            guard value.objectValue != nil else {
                throw GatewayWireError.invalidFrame("\(type) has invalid context")
            }
            for key in ["tenantId", "userId", "userName", "workspaceId", "workspaceLabel", "originLabel"] {
                try optionalString(key, in: value)
            }
        }

        func validateModel(_ value: JSONValue) throws {
            guard value.objectValue != nil else {
                throw GatewayWireError.invalidFrame("\(type) has invalid model")
            }
            try requireString("route", in: value)
            try requireString("model", in: value)
            try optionalString("reasoningEffort", in: value)
            try optionalInteger("modelContextWindow", in: value)
        }

        func validatePhase() throws {
            guard let phase = msg["phase"], phase != .null else { return }
            guard let value = phase.stringValue,
                  ["commentary", "final_answer"].contains(value)
            else {
                throw GatewayWireError.invalidFrame("\(type) has invalid phase")
            }
        }

        func validateMessageTarget() throws {
            guard let value = msg["messageTarget"],
                  value == .null || MessageTarget(json: value) != nil
            else {
                throw GatewayWireError.invalidFrame("\(type) has invalid message target")
            }
        }

        func validateUsage(_ value: JSONValue) throws {
            guard value.objectValue != nil else {
                throw GatewayWireError.invalidFrame("\(type) has invalid token usage")
            }
            for key in [
                "inputTokens", "cachedInputTokens", "outputTokens",
                "cacheWriteInputTokens", "reasoningOutputTokens", "totalTokens"
            ] {
                try requireInteger(key, in: value)
            }
        }

        func validateAttachments() throws {
            guard let attachments = msg["attachments"]?.arrayValue,
                  attachments.count <= maximumAttachmentReferences
            else {
                throw GatewayWireError.invalidFrame("\(type) has invalid attachments")
            }
            try attachments.forEach { _ = try AttachmentRecord(json: $0) }
        }

        switch type {
        case "error", "warning":
            try requireString("message")
        case "user_message":
            try requireString("message")
            try validateAttachments()
            try validateMessageTarget()
        case "session_configured":
            try requireString("sessionId")
            guard let context = msg["context"], let model = msg["model"] else {
                throw GatewayWireError.invalidFrame("session_configured is missing context or model")
            }
            try validateContext(context)
            try validateModel(model)
        case "task_started":
            try requireString("turnId")
            try optionalInteger("modelContextWindow")
        case "task_complete":
            try requireString("turnId")
            try optionalString("lastAgentMessage")
        case "turn_aborted":
            try requireString("turnId")
            try requireString("reason")
        case "agent_message":
            try requireString("message")
            try validatePhase()
            try validateMessageTarget()
        case "agent_message_content_delta":
            for key in ["threadId", "turnId", "itemId", "delta"] { try requireString(key) }
            try validatePhase()
        case "agent_reasoning_content_delta":
            for key in ["threadId", "turnId", "itemId", "delta"] { try requireString(key) }
        case "session_history":
            guard let events = msg["events"]?.arrayValue else {
                throw GatewayWireError.invalidFrame("session history is missing events")
            }
            try events.forEach(validate)
        case "model_changed":
            try validateModel(msg)
        case "session_resume_requested":
            try requireString("sessionId")
            guard let context = msg["context"] else {
                throw GatewayWireError.invalidFrame("session_resume_requested has invalid context")
            }
            try validateContext(context)
        case "tool_call_begin":
            for key in ["turnId", "callId", "name"] { try requireString(key) }
            guard msg["arguments"] != nil else {
                throw GatewayWireError.invalidFrame("tool_call_begin has invalid arguments")
            }
        case "tool_call_end":
            for key in ["turnId", "callId", "name", "output"] { try requireString(key) }
            try requireBool("isError")
        case "exec_approval_request":
            for key in ["id", "turnId", "reason"] { try requireString(key) }
            guard let calls = msg["calls"]?.arrayValue else {
                throw GatewayWireError.invalidFrame("exec_approval_request has invalid calls")
            }
            for call in calls {
                try requireString("callId", in: call)
                try requireString("name", in: call)
                guard call["arguments"] != nil else {
                    throw GatewayWireError.invalidFrame("exec_approval_request has invalid arguments")
                }
            }
        case "token_count":
            if let info = msg["info"], info != .null {
                guard info.objectValue != nil,
                      let total = info["totalTokenUsage"],
                      let last = info["lastTokenUsage"]
                else {
                    throw GatewayWireError.invalidFrame("token_count has invalid info")
                }
                try validateUsage(total)
                try validateUsage(last)
                try optionalInteger("modelContextWindow", in: info)
            }
        case "context_compacted":
            break
        case "web_search_begin":
            try requireString("callId")
        case "web_search_end":
            try requireString("callId")
            try optionalString("query")
            try requireString("action")
        case "frontend":
            guard let frontendType = msg["frontendType"]?.stringValue else {
                throw GatewayWireError.invalidFrame("frontend event has no frontend_type")
            }
            switch frontendType {
            case "render":
                guard msg["capability"]?.stringValue != nil, let block = msg["block"] else {
                    throw GatewayWireError.invalidFrame("frontend render is missing a required field")
                }
                _ = try FrontendBlock(json: block)
            case "widget":
                guard msg["capability"]?.stringValue != nil, let item = msg["item"] else {
                    throw GatewayWireError.invalidFrame("frontend widget is missing a required field")
                }
                _ = try FrontendWidget(json: item)
            case "remove_widget":
                guard msg["capability"]?.stringValue != nil, msg["id"]?.stringValue != nil else {
                    throw GatewayWireError.invalidFrame("frontend remove_widget is missing a required field")
                }
            case "picker":
                guard msg["title"]?.stringValue != nil, let options = msg["options"]?.arrayValue else {
                    throw GatewayWireError.invalidFrame("frontend picker is missing a required field")
                }
                try options.forEach { _ = try FrontendPickerOption(json: $0) }
            case "preview":
                guard msg["title"]?.stringValue != nil, let events = msg["events"]?.arrayValue else {
                    throw GatewayWireError.invalidFrame("frontend preview is missing a required field")
                }
                try events.forEach(validate)
            default:
                throw GatewayWireError.invalidFrame("unknown frontend event \(frontendType)")
            }
        default:
            throw GatewayWireError.invalidFrame("unknown agent event \(type)")
        }
    }
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
    var provider: ProviderConfig
    var middleware: MiddlewareConfig
    var systemPrompt: String
}

struct ProviderConfig: Codable, Equatable, Sendable {
    var provider: String
    var model: String
    var baseUrl: String?
    var reasoningEffort: String?
    var webSearch: HostedWebSearch
}

enum HostedWebSearch: String, Codable, CaseIterable, Identifiable, Sendable {
    case off
    case cached
    case live

    var id: Self { self }

    var label: String {
        switch self {
        case .off: "Off"
        case .cached: "Cached"
        case .live: "Live"
        }
    }
}

struct MiddlewareConfig: Codable, Equatable, Sendable {
    var enabled: Set<String>
    var settings: [String: [String: FrontendSettingValue]]
}

extension MiddlewareConfig {
    mutating func setSetting(
        _ value: FrontendSettingValue?,
        middleware: String,
        setting: String
    ) {
        if let value {
            settings[middleware, default: [:]][setting] = value
        } else {
            settings[middleware]?[setting] = nil
            if settings[middleware]?.isEmpty == true { settings[middleware] = nil }
        }
    }
}

enum FrontendSettingValue: Codable, Equatable, Sendable {
    case integer(Int64)
    case string(String)

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let value = try? container.decode(Int64.self) {
            self = .integer(value)
        } else {
            self = .string(try container.decode(String.self))
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .integer(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        }
    }
}

struct MiddlewareFeature: Identifiable, Decodable, Equatable, Sendable {
    let id: String
    let label: String
    let description: String
    let required: Bool
    let settings: [FrontendSetting]
}

struct FrontendSetting: Identifiable, Decodable, Equatable, Sendable {
    let id: String
    let label: String
    let description: String
    let kind: FrontendSettingKind

    init(id: String, label: String, description: String, kind: FrontendSettingKind) {
        self.id = id
        self.label = label
        self.description = description
        self.kind = kind
    }

    private enum CodingKeys: String, CodingKey {
        case id, label, description, type, min, max, step, options, unsetLabel
    }

    private enum Kind: String, Decodable {
        case integer
        case select
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        description = try container.decode(String.self, forKey: .description)
        switch try container.decode(Kind.self, forKey: .type) {
        case .integer:
            let minimum = try container.decode(Int64.self, forKey: .min)
            let maximum = try container.decodeIfPresent(Int64.self, forKey: .max)
            let step = try container.decode(Int64.self, forKey: .step)
            guard maximum.map({ $0 >= minimum }) ?? true else {
                throw GatewayWireError.invalidFrame(
                    "frontend integer setting maximum is below minimum"
                )
            }
            guard step > 0 else {
                throw GatewayWireError.invalidFrame(
                    "frontend integer setting step must be positive"
                )
            }
            kind = .integer(
                min: minimum,
                max: maximum,
                step: step
            )
        case .select:
            let options = try container.decode([FrontendSettingOption].self, forKey: .options)
            guard Set(options.map(\.value)).count == options.count else {
                throw GatewayWireError.invalidFrame(
                    "frontend select setting has duplicate option values"
                )
            }
            kind = .select(
                options: options,
                unsetLabel: try container.decodeIfPresent(String.self, forKey: .unsetLabel)
            )
        }
    }
}

enum FrontendSettingKind: Equatable, Sendable {
    case integer(min: Int64, max: Int64?, step: Int64)
    case select(options: [FrontendSettingOption], unsetLabel: String?)
}

struct FrontendSettingOption: Identifiable, Decodable, Equatable, Sendable {
    var id: String { value }

    let value: String
    let label: String
    let description: String
}

struct ProviderStatus: Identifiable, Codable, Equatable, Sendable {
    var id: String { provider }

    let provider: String
    let label: String
    let symbol: String
    let description: String
    var configured: Bool
    let selection: ProviderConfig?
    let auth: ProviderAuthKind
    let defaultBaseUrl: String?
    let defaultApiKeyEnv: String?
    let models: [ProviderModel]
    let modelIds: [String]
    let modelIdsConfigurable: Bool
    let webSearch: [HostedWebSearch]
}

enum GatewayClientKind: String, Codable, Sendable {
    case cli
    case macos
    case ios
    case ipados
    case gatewayDashboard = "gateway_dashboard"

    @MainActor static var currentApplePlatform: Self {
        #if os(macOS)
        .macos
        #elseif os(iOS)
        UIDevice.current.userInterfaceIdiom == .pad ? .ipados : .ios
        #else
        #error("Unsupported Horus Apple platform")
        #endif
    }
}

struct ClientStatus: Codable, Equatable, Sendable {
    let clientId: String
    let label: String
    let kinds: [GatewayClientKind]
    let connections: Int
}

enum ProviderAuthKind: String, Codable, Sendable {
    case apiKey = "api_key"
    case deviceCode = "device_code"
}

struct ProviderModel: Identifiable, Codable, Equatable, Sendable {
    let id: String
    let label: String
    let description: String
    let contextWindow: Int64
    let reasoning: [ReasoningChoice]
    let defaultReasoning: String?
}

struct ReasoningChoice: Identifiable, Codable, Equatable, Sendable {
    let id: String
    let label: String
    let description: String
}

struct TokenUsage: Codable, Hashable, Sendable {
    var inputTokens = 0
    var cachedInputTokens = 0
    var cacheWriteInputTokens = 0
    var outputTokens = 0
    var reasoningOutputTokens = 0
    var totalTokens = 0
}

struct ProfileSnapshot: Codable, Equatable, Sendable {
    let userName: String?
    let dailyUsage: [DailyUsage]
    let runStats: RunStats
    let recentRunGroups: [SessionRunGroup]
}

struct SessionRunGroup: Identifiable, Codable, Equatable, Sendable {
    var id: String { sessionId }

    let sessionId: String
    let title: String
    let runs: [RunSummary]
}

struct ExecutionStats: Codable, Hashable, Sendable {
    var runCount: UInt64 = 0
    var failedRunCount: UInt64 = 0
    var abortedRunCount: UInt64 = 0
    var modelCalls: UInt64 = 0
    var toolCalls: UInt64 = 0
    var failedToolCalls: UInt64 = 0
    var elapsedMs: UInt64 = 0
    var usage = TokenUsage()
}

struct RunStats: Codable, Equatable, Sendable {
    var runCount: UInt64 = 0
    var failedRunCount: UInt64 = 0
    var abortedRunCount: UInt64 = 0
    var modelCalls: UInt64 = 0
    var toolCalls: UInt64 = 0
    var failedToolCalls: UInt64 = 0
    var elapsedMs: UInt64 = 0
    var usage = TokenUsage()
    var active: RunSummary? = nil
}

struct RunSummary: Identifiable, Codable, Equatable, Sendable {
    var id: String { "\(sessionId):\(turnId)" }

    let sessionId: String
    let submissionId: String
    let turnId: String
    let startedAtMs: Int64
    let finishedAtMs: Int64?
    let elapsedMs: UInt64
    let outcome: SessionOutcome?
    let modelCalls: UInt64
    var toolCalls: UInt64
    var failedToolCalls: UInt64
    let usage: TokenUsage
}

struct DailyUsage: Codable, Equatable, Sendable {
    let unixDay: UInt64
    let usage: TokenUsage
}

struct CronTask: Identifiable, Codable, Equatable, Sendable {
    let id: String
    let sessionId: String
    let task: String
    let schedule: String
}

struct CronRun: Identifiable, Codable, Equatable, Sendable {
    let id: String
    let taskId: String
    let sourceSessionId: String
    let startedAt: Int64
    let finishedAt: Int64?
    let status: CronRunStatus
    let sessionId: String?
    let message: String?
}

enum CronRunStatus: String, Codable, Equatable, Sendable {
    case running
    case succeeded
    case failed
    case skipped
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
