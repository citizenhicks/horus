import Foundation
import StoreKit

enum MobiusCloudAction: Equatable {
    case idle
    case signingIn
    case purchasing
    case restoring
    case provisioning

    var isRunning: Bool { self != .idle }

    var label: String {
        switch self {
        case .idle: ""
        case .signingIn: "Signing in…"
        case .purchasing: "Confirming purchase…"
        case .restoring: "Restoring purchases…"
        case .provisioning: "Setting up your gateway…"
        }
    }
}

extension AppModel {
    func signInAndPurchaseCloud(
        authorizationCode: String,
        nonce: String,
        product: Product
    ) async -> Bool {
        guard cloudAction == .idle else { return false }
        cloudAction = .signingIn
        cloudError = nil
        defer { cloudAction = .idle }

        do {
            cloudSession = try await cloudClient.authenticate(
                authorizationCode: authorizationCode,
                nonce: nonce
            )
            cloudAction = .purchasing
            return try await purchaseAndProvision(product)
        } catch is CancellationError {
            return false
        } catch {
            reportCloud(error)
            return false
        }
    }

    func purchaseCloud(_ product: Product) async -> Bool {
        guard cloudAction == .idle else { return false }
        guard cloudSession != nil else {
            reportCloud(MobiusCloudError.authenticationRequired)
            return false
        }
        cloudAction = .purchasing
        cloudError = nil
        defer { cloudAction = .idle }

        do {
            return try await purchaseAndProvision(product)
        } catch is CancellationError {
            return false
        } catch {
            reportCloud(error)
            return false
        }
    }

    func restoreCloudPurchases() async -> Bool {
        guard cloudAction == .idle else { return false }
        guard cloudSession != nil else {
            reportCloud(MobiusCloudError.authenticationRequired)
            return false
        }
        cloudAction = .restoring
        cloudError = nil
        defer { cloudAction = .idle }

        do {
            try await AppStore.sync()
            var sawUnverifiedTransaction = false
            for await verification in Transaction.currentEntitlements(
                for: mobiusCloudMonthlyProductID
            ) {
                switch verification {
                case .verified:
                    try await acknowledge(verification)
                    cloudAction = .provisioning
                    try await provisionAndPairCloudGateway()
                    showToast("Purchases restored.", tone: .success)
                    return true
                case .unverified:
                    sawUnverifiedTransaction = true
                }
            }
            throw sawUnverifiedTransaction
                ? MobiusCloudError.unverifiedTransaction
                : MobiusCloudError.invalidSignedTransaction
        } catch is CancellationError {
            return false
        } catch {
            reportCloud(error)
            return false
        }
    }

    func reportCloudSignInFailure() {
        reportCloud(MobiusCloudError.invalidAuthorization)
    }

    private func purchaseAndProvision(_ product: Product) async throws -> Bool {
        guard product.id == mobiusCloudMonthlyProductID,
              let userID = cloudSession?.userID
        else { throw MobiusCloudError.invalidSignedTransaction }

        switch try await product.purchase(options: [.appAccountToken(userID)]) {
        case .success(let verification):
            try await acknowledge(verification)
            cloudAction = .provisioning
            try await provisionAndPairCloudGateway()
            showToast("Your Cloud gateway is ready to pair.", tone: .success)
            return true
        case .pending:
            showToast("Purchase approval is pending.", tone: .info)
            return false
        case .userCancelled:
            return false
        @unknown default:
            throw MobiusCloudError.unverifiedTransaction
        }
    }

    private func acknowledge(_ verification: VerificationResult<Transaction>) async throws {
        guard case .verified(let transaction) = verification,
              let session = cloudSession,
              transaction.productID == mobiusCloudMonthlyProductID,
              transaction.appAccountToken == session.userID,
              transaction.revocationDate == nil,
              transaction.expirationDate.map({ $0 > .now }) ?? true
        else { throw MobiusCloudError.unverifiedTransaction }

        try await cloudClient.submitSubscription(
            signedTransaction: verification.jwsRepresentation
        )
        await transaction.finish()
    }

    private func provisionAndPairCloudGateway() async throws {
        for attempt in 0..<150 {
            switch try await cloudClient.gatewayStatus() {
            case .waiting:
                guard attempt < 149 else { throw MobiusCloudError.provisioningTimedOut }
                try await Task.sleep(for: .seconds(2))
            case .ready:
                let grant = try await cloudClient.createPairingGrant()
                applyPairingSetup(grant.setup)
                pair()
                return
            case .error:
                throw MobiusCloudError.provisioningFailed
            }
        }
    }

    private func reportCloud(_ error: Error) {
        if let cloudError = error as? MobiusCloudError {
            switch cloudError {
            case .authenticationRequired, .sessionExpired, .server(401):
                cloudSession = nil
            default:
                break
            }
        }
        let message = (error as? MobiusCloudError)?.localizedDescription
            ?? "Couldn’t connect to möbius Cloud. Try again."
        cloudError = message
        showToast(message, tone: .error)
    }
}
