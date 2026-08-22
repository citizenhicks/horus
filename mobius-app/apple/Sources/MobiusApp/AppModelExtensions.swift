import Foundation

private let extensionAuthorizationRedirectURI = "mobius://extension-auth"

extension AppModel {
    /// One extension mutation is in flight at a time, and all of them need the gateway.
    var canMutateExtensions: Bool {
        extensionAction == nil && connectionState.isReady
    }

    func installExtension() {
        let source = extensionInstallSource.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !source.isEmpty else { return }
        beginExtensionAction(.installing) { requestID in
            .installExtension(
                requestID: requestID,
                source: source,
                reference: nil,
                subdirectory: nil
            )
        }
    }

    func installExtension(_ item: MobiusCloudExtensionCatalogItem) {
        guard availableExtensions.contains(item) else { return }
        beginExtensionAction(.installing) { requestID in
            .installExtension(
                requestID: requestID,
                source: item.source.url,
                reference: item.source.reference,
                subdirectory: item.source.subdirectory
            )
        }
    }

    func refreshExtensionCatalog() async {
        guard let userID = cloudSession?.userID else {
            availableExtensions = []
            extensionCatalogError = nil
            isLoadingExtensionCatalog = false
            return
        }
        availableExtensions = []
        extensionCatalogError = nil
        isLoadingExtensionCatalog = true
        defer {
            if cloudSession?.userID == userID { isLoadingExtensionCatalog = false }
        }

        do {
            let catalog = try await cloudClient.extensionCatalog()
            guard cloudSession?.userID == userID else { return }
            availableExtensions = catalog
        } catch is CancellationError {
            return
        } catch {
            guard cloudSession?.userID == userID else { return }
            if let error = error as? MobiusCloudError {
                switch error {
                case .authenticationRequired, .sessionExpired, .server(401):
                    reportCloud(error)
                    return
                default:
                    break
                }
            }
            extensionCatalogError = (error as? MobiusCloudError)?.localizedDescription
                ?? "The extension catalog is temporarily unavailable."
        }
    }

    func updateExtension(_ extensionRecord: ExtensionRecord) {
        beginExtensionAction(.updating(extensionRecord.name)) { requestID in
            .updateExtension(requestID: requestID, id: extensionRecord.id)
        }
    }

    func uninstallExtension(_ extensionRecord: ExtensionRecord) {
        beginExtensionAction(.uninstalling(extensionRecord.name)) { requestID in
            .uninstallExtension(requestID: requestID, id: extensionRecord.id)
        }
    }

    func trustHooks(for extensionRecord: ExtensionRecord) {
        guard !extensionRecord.hooks.isEmpty, !extensionRecord.hooksTrusted else { return }
        beginExtensionAction(.trusting(extensionRecord.name)) { requestID in
            .trustExtensionHooks(
                requestID: requestID,
                id: extensionRecord.id,
                expectedDigest: extensionRecord.digest
            )
        }
    }

    func untrustHooks(for extensionRecord: ExtensionRecord) {
        guard !extensionRecord.hooks.isEmpty, extensionRecord.hooksTrusted else { return }
        beginExtensionAction(.untrusting(extensionRecord.name)) { requestID in
            .revokeExtensionHooksTrust(
                requestID: requestID,
                id: extensionRecord.id,
                expectedDigest: extensionRecord.digest
            )
        }
    }

    func startExtensionConnection(_ extensionRecord: ExtensionRecord) {
        guard extensionRecord.connection?.kind == .oauth else { return }
        beginExtensionAction(.connecting(
            id: extensionRecord.id,
            name: extensionRecord.name
        )) { requestID in
            .startExtensionConnection(
                requestID: requestID,
                id: extensionRecord.id,
                redirectURI: extensionAuthorizationRedirectURI
            )
        }
    }

    func setExtensionConnectionSecret(_ extensionRecord: ExtensionRecord, secret: String) {
        guard extensionRecord.connection?.kind == .apiKey,
              !secret.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { return }
        beginExtensionAction(.connecting(
            id: extensionRecord.id,
            name: extensionRecord.name
        )) { requestID in
            .setExtensionConnectionSecret(
                requestID: requestID,
                id: extensionRecord.id,
                secret: secret
            )
        }
    }

    func disconnectExtension(_ extensionRecord: ExtensionRecord) {
        guard extensionRecord.connection?.state == .connected else { return }
        beginExtensionAction(.disconnecting(extensionRecord.name)) { requestID in
            .disconnectExtensionConnection(requestID: requestID, id: extensionRecord.id)
        }
    }

    func receiveExtensionAuthorization(
        requestID: String,
        extensionID: String,
        authorizationURL: String
    ) {
        guard requestID == extensionRequestID else { return }
        guard case .connecting(let expectedID, _) = extensionAction,
              extensionID == expectedID,
              extensions.contains(where: { $0.id == extensionID }),
              let url = URL(string: authorizationURL),
              url.scheme?.lowercased() == "https",
              url.host != nil,
              url.user == nil,
              url.password == nil
        else {
            rejectExtensionAction(requestID: requestID)
            showToast("The extension returned an invalid sign-in address.", tone: .error)
            return
        }
        extensionAuthorizationChallenge = ExtensionAuthorizationChallenge(
            id: requestID,
            extensionID: extensionID,
            authorizationURL: url
        )
    }

    func finishExtensionConnection(_ challenge: ExtensionAuthorizationChallenge, callbackURL: URL) {
        guard challenge.id == extensionRequestID,
              extensionAuthorizationChallenge == challenge,
              callbackURL.scheme?.lowercased() == "mobius",
              callbackURL.host?.lowercased() == "extension-auth",
              callbackURL.user == nil,
              callbackURL.password == nil,
              callbackURL.port == nil,
              callbackURL.path.isEmpty || callbackURL.path == "/",
              connectionState.isReady
        else {
            failExtensionConnection(challenge.id)
            return
        }
        extensionAuthorizationChallenge = nil
        let id = requestID("extension-connection-finish")
        extensionRequestID = id
        transmit(.finishExtensionConnection(
            requestID: id,
            id: challenge.extensionID,
            callbackURL: callbackURL.absoluteString
        )) { [weak self] _ in
            self?.rejectExtensionAction(requestID: id)
        }
    }

    func cancelExtensionConnection(_ requestID: String) {
        guard requestID == extensionRequestID else { return }
        rejectExtensionAction(requestID: requestID)
        showToast("Connection canceled.", tone: .info)
    }

    func failExtensionConnection(_ requestID: String) {
        guard requestID == extensionRequestID else { return }
        rejectExtensionAction(requestID: requestID)
        showToast("The extension sign-in could not be completed.", tone: .error)
    }

    func completeExtensionAction(requestID: String) {
        guard requestID == extensionRequestID, let action = extensionAction else { return }
        extensionRequestID = nil
        extensionAction = nil
        if action == .installing { extensionInstallSource = "" }
        let outcome = extensionCompletionOutcome(action)
        showToast(outcome.message, tone: outcome.tone)
    }

    func rejectExtensionAction(requestID: String) {
        guard requestID == extensionRequestID else { return }
        if extensionAuthorizationChallenge?.id == requestID {
            extensionAuthorizationChallenge = nil
        }
        extensionRequestID = nil
        extensionAction = nil
    }

    private func beginExtensionAction(
        _ action: ExtensionAction,
        request: (String) -> GatewayRequest
    ) {
        guard extensionRequestID == nil, connectionState.isReady else { return }
        let id = requestID("extension")
        extensionRequestID = id
        extensionAction = action
        transmit(request(id)) { [weak self] _ in
            self?.rejectExtensionAction(requestID: id)
        }
    }

    private func extensionCompletionOutcome(
        _ action: ExtensionAction
    ) -> (message: String, tone: ToastTone) {
        switch action {
        case .installing:
            return ("Extension installed.", .success)
        case .updating(let name):
            return ("\(name) updated.", .success)
        case .uninstalling(let name):
            return ("\(name) uninstalled.", .success)
        case .trusting(let name):
            return ("\(name) hooks trusted.", .success)
        case .untrusting(let name):
            return ("\(name) hooks untrusted.", .success)
        case .connecting(let id, let name):
            guard let connection = extensions.first(where: { $0.id == id })?.connection,
                  connection.state == .connected
            else { return ("\(name) connection was not completed.", .info) }
            return connection.kind == .apiKey
                ? ("\(name) credential saved.", .success)
                : ("\(name) connected.", .success)
        case .disconnecting(let name):
            return ("\(name) disconnected.", .success)
        }
    }
}
