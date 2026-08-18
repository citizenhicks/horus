import Foundation

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

    func completeExtensionAction(requestID: String) {
        guard requestID == extensionRequestID, let action = extensionAction else { return }
        extensionRequestID = nil
        extensionAction = nil
        if action == .installing { extensionInstallSource = "" }
        showToast(extensionSuccessMessage(action), tone: .success)
    }

    func rejectExtensionAction(requestID: String) {
        guard requestID == extensionRequestID else { return }
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

    private func extensionSuccessMessage(_ action: ExtensionAction) -> String {
        switch action {
        case .installing: "Extension installed."
        case .updating(let name): "\(name) updated."
        case .uninstalling(let name): "\(name) uninstalled."
        case .trusting(let name): "\(name) hooks trusted."
        case .untrusting(let name): "\(name) hooks untrusted."
        }
    }
}
