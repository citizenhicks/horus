import Foundation
import Security

@MainActor
final class GatewayStore {
    private let defaults: UserDefaults
    private let accountsKey = "paired-gateways"
    private let selectedAccountKey = "selected-gateway"
    private let keychainService = "app.horus.gateway"
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
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

    func lastSequence(for account: GatewayAccount) -> UInt64? {
        let value = defaults.object(forKey: sequenceKey(account.id)) as? NSNumber
        return value?.uint64Value
    }

    func saveLastSequence(_ sequence: UInt64, for account: GatewayAccount) {
        defaults.set(NSNumber(value: sequence), forKey: sequenceKey(account.id))
    }

    func clearLastSequence(for account: GatewayAccount) {
        defaults.removeObject(forKey: sequenceKey(account.id))
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
        defaults.removeObject(forKey: sequenceKey(account.id))
        if selectedAccountID() == account.id {
            defaults.removeObject(forKey: selectedAccountKey)
        }
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

    private func sequenceKey(_ accountID: UUID) -> String {
        "gateway-sequence-\(accountID.uuidString)"
    }
}

extension GatewayStore {
    enum StoreError: LocalizedError {
        case invalidToken
        case missingToken
        case keychain(OSStatus)

        var errorDescription: String? {
            switch self {
            case .invalidToken: "The gateway token is invalid."
            case .missingToken: "This gateway needs to be paired again."
            case .keychain(let status):
                SecCopyErrorMessageString(status, nil) as String?
                    ?? "Keychain operation failed (\(status))."
            }
        }
    }
}
