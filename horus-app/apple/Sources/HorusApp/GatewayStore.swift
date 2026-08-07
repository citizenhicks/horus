import Foundation
import Security

struct CachedTranscript: Codable {
    private struct Entry: Codable {
        let id: String
        let text: String
        let kind: TranscriptEntry.Kind
        let group: String?
        let format: String
        let tone: String
        let pending: Bool
        let messageTarget: MessageTarget?
        let attachments: [AttachmentRecord]

        init(_ entry: TranscriptEntry) {
            id = entry.id
            text = entry.text
            kind = entry.kind
            group = entry.group
            format = entry.format
            tone = entry.tone
            pending = entry.pending
            messageTarget = entry.messageTarget
            attachments = entry.attachments
        }

        var transcriptEntry: TranscriptEntry {
            TranscriptEntry(
                id: id,
                text: text,
                kind: kind,
                group: group,
                format: format,
                tone: tone,
                pending: pending,
                messageTarget: messageTarget,
                attachments: attachments
            )
        }
    }

    let replayEpoch: String
    let sequence: UInt64
    let currentUsage: TokenUsage
    let lastUsage: TokenUsage
    private let entries: [Entry]

    init(
        replayEpoch: String,
        sequence: UInt64,
        transcript: [TranscriptEntry],
        currentUsage: TokenUsage,
        lastUsage: TokenUsage
    ) {
        self.replayEpoch = replayEpoch
        self.sequence = sequence
        self.currentUsage = currentUsage
        self.lastUsage = lastUsage
        entries = transcript.map(Entry.init)
    }

    var transcript: [TranscriptEntry] { entries.map(\.transcriptEntry) }
}

@MainActor
final class GatewayStore {
    private let maximumCachedTranscriptsPerAccount = 20
    private let maximumCachedTranscriptBytes = 4 * 1024 * 1024
    private let maximumCachedTranscriptContentBytes = 3 * 1024 * 1024
    private let maximumCachedTranscriptEntries = 10_000
    private let defaults: UserDefaults
    private let transcriptDirectory: URL
    private let accountsKey = "paired-gateways"
    private let selectedAccountKey = "selected-gateway"
    private let keychainService = "app.horus.gateway"
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(defaults: UserDefaults = .standard, transcriptDirectory: URL? = nil) {
        self.defaults = defaults
        self.transcriptDirectory = transcriptDirectory
            ?? URL.cachesDirectory
                .appendingPathComponent("Horus", isDirectory: true)
                .appendingPathComponent("Transcripts", isDirectory: true)
    }

    func loadAccounts() -> [GatewayAccount] {
        guard let data = defaults.data(forKey: accountsKey),
              let accounts = try? decoder.decode([GatewayAccount].self, from: data)
        else { return [] }
        return accounts
    }

    func save(_ account: GatewayAccount, token: String) throws {
        try saveToken(token, accountID: account.id)
        var accounts = loadAccounts()
        if let index = accounts.firstIndex(where: { $0.id == account.id }) {
            accounts[index] = account
        } else {
            accounts.append(account)
        }
        defaults.set(try encoder.encode(accounts), forKey: accountsKey)
        defaults.set(account.id.uuidString, forKey: selectedAccountKey)
    }

    func selectedAccountID() -> UUID? {
        defaults.string(forKey: selectedAccountKey).flatMap(UUID.init(uuidString:))
    }

    func select(_ account: GatewayAccount) {
        defaults.set(account.id.uuidString, forKey: selectedAccountKey)
    }

    func rename(_ account: GatewayAccount, to rawName: String) throws -> GatewayAccount {
        let name = rawName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty,
              name.utf8.count <= 128,
              !name.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
        else { throw StoreError.invalidDisplayName }

        var accounts = loadAccounts()
        guard let index = accounts.firstIndex(where: { $0.id == account.id }) else {
            throw StoreError.missingAccount
        }
        var renamed = account
        renamed.displayName = name
        accounts[index] = renamed
        defaults.set(try encoder.encode(accounts), forKey: accountsKey)
        return renamed
    }

    func token(for account: GatewayAccount) throws -> String {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: keychainService,
            kSecAttrAccount: account.id.uuidString,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status != errSecItemNotFound else { throw StoreError.missingToken }
        guard status == errSecSuccess,
              let data = result as? Data,
              let token = String(data: data, encoding: .utf8)
        else {
            throw StoreError.keychain(status)
        }
        return token
    }

    func remove(_ account: GatewayAccount) throws {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: keychainService,
            kSecAttrAccount: account.id.uuidString,
        ]
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw StoreError.keychain(status)
        }
        let accounts = loadAccounts().filter { $0.id != account.id }
        defaults.set(try encoder.encode(accounts), forKey: accountsKey)
        if selectedAccountID() == account.id {
            defaults.removeObject(forKey: selectedAccountKey)
        }
        try? FileManager.default.removeItem(at: accountTranscriptDirectory(account.id))
    }

    func loadTranscript(accountID: UUID, sessionID: String) -> CachedTranscript? {
        let url = transcriptURL(accountID: accountID, sessionID: sessionID)
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: url.path),
              let size = (attributes[.size] as? NSNumber)?.intValue
        else { return nil }
        guard size <= maximumCachedTranscriptBytes,
              let data = try? Data(contentsOf: url),
              let cached = try? decoder.decode(CachedTranscript.self, from: data)
        else {
            try? FileManager.default.removeItem(at: url)
            return nil
        }
        return cached
    }

    func saveTranscript(
        accountID: UUID,
        sessionID: String,
        replayEpoch: String,
        sequence: UInt64,
        transcript: [TranscriptEntry],
        currentUsage: TokenUsage,
        lastUsage: TokenUsage
    ) {
        guard !transcript.isEmpty else { return }
        let url = transcriptURL(accountID: accountID, sessionID: sessionID)
        guard transcriptFitsCache(transcript) else {
            try? FileManager.default.removeItem(at: url)
            return
        }
        let directory = accountTranscriptDirectory(accountID)
        try? FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        guard let data = try? encoder.encode(CachedTranscript(
            replayEpoch: replayEpoch,
            sequence: sequence,
            transcript: transcript,
            currentUsage: currentUsage,
            lastUsage: lastUsage
        )) else { return }
        guard data.count <= maximumCachedTranscriptBytes else {
            try? FileManager.default.removeItem(at: url)
            return
        }
        trimTranscriptCache(in: directory, keeping: url)
        #if os(iOS)
        let options: Data.WritingOptions = [.atomic, .completeFileProtection]
        #else
        let options: Data.WritingOptions = .atomic
        #endif
        try? data.write(
            to: url,
            options: options
        )
    }

    func removeTranscript(accountID: UUID, sessionID: String) {
        try? FileManager.default.removeItem(
            at: transcriptURL(accountID: accountID, sessionID: sessionID)
        )
    }

    private func saveToken(_ token: String, accountID: UUID) throws {
        guard let data = token.data(using: .utf8) else { throw StoreError.invalidToken }
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: keychainService,
            kSecAttrAccount: accountID.uuidString,
        ]
        let attributes: [CFString: Any] = [
            kSecValueData: data,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        let updateStatus = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        guard updateStatus == errSecItemNotFound else {
            guard updateStatus == errSecSuccess else { throw StoreError.keychain(updateStatus) }
            return
        }

        let item = query.merging(attributes) { _, new in new }
        let addStatus = SecItemAdd(item as CFDictionary, nil)
        guard addStatus == errSecSuccess else { throw StoreError.keychain(addStatus) }
    }

    private func accountTranscriptDirectory(_ accountID: UUID) -> URL {
        transcriptDirectory.appendingPathComponent(accountID.uuidString, isDirectory: true)
    }

    private func transcriptURL(accountID: UUID, sessionID: String) -> URL {
        let filename = Data(sessionID.utf8).base64EncodedString()
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "+", with: "-")
        return accountTranscriptDirectory(accountID)
            .appendingPathComponent(filename)
            .appendingPathExtension("json")
    }

    private func trimTranscriptCache(in directory: URL, keeping url: URL) {
        let cached = (try? FileManager.default.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: nil
        ))?
            .filter { $0.pathExtension == "json" && $0 != url }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
            ?? []
        for stale in cached.dropFirst(maximumCachedTranscriptsPerAccount - 1) {
            try? FileManager.default.removeItem(at: stale)
        }
    }

    private func transcriptFitsCache(_ transcript: [TranscriptEntry]) -> Bool {
        guard transcript.count <= maximumCachedTranscriptEntries else { return false }
        var remaining = maximumCachedTranscriptContentBytes
        func consume(_ value: String?) -> Bool {
            guard let value else { return true }
            let count = value.utf8.count
            guard count <= remaining else { return false }
            remaining -= count
            return true
        }
        for entry in transcript {
            guard consume(entry.id),
                  consume(entry.text),
                  consume(entry.group),
                  consume(entry.format),
                  consume(entry.tone),
                  entry.attachments.allSatisfy({ attachment in
                      consume(attachment.id)
                          && consume(attachment.name)
                          && consume(attachment.mediaType)
                  })
            else { return false }
        }
        return true
    }
}

extension GatewayStore {
    enum StoreError: LocalizedError {
        case invalidDisplayName
        case missingAccount
        case invalidToken
        case missingToken
        case keychain(OSStatus)

        var errorDescription: String? {
            switch self {
            case .invalidDisplayName: "Use a gateway name between 1 and 128 characters."
            case .missingAccount: "This gateway is no longer saved."
            case .invalidToken: "The gateway token is invalid."
            case .missingToken: "This gateway needs to be paired again."
            case .keychain(let status):
                SecCopyErrorMessageString(status, nil) as String?
                    ?? "Keychain operation failed (\(status))."
            }
        }
    }
}
