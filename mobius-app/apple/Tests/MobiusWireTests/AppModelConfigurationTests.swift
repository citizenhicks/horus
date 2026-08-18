import Foundation
import XCTest

@MainActor
extension AppModelTests {
    func testEmptyGatewayCanRegisterItsFirstProviderWithoutAChat() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        model.applyGatewayCatalog(ReadyPayload(
            machineName: "snowwhite.local",
            sessions: [],
            providers: [ProviderStatus(
                provider: "openai_socket",
                label: "OpenAI",
                symbol: "chat_gpt",
                description: "Persistent Responses API",
                configured: true,
                selection: nil,
                auth: .apiKey,
                defaultBaseUrl: nil,
                defaultApiKeyEnv: "OPENAI_API_KEY",
                models: [ProviderModel(
                    id: "gpt-5.6-sol",
                    label: "Sol",
                    description: "Frontier capability",
                    contextWindow: 1_050_000,
                    reasoning: [ReasoningChoice(
                        id: "high",
                        label: "High",
                        description: "Deep reasoning"
                    )],
                    defaultReasoning: "high"
                )],
                modelIds: [],
                reasoningEfforts: [],
                modelIdsConfigurable: false,
                webSearch: [.off, .cached, .live]
            )],
            defaultConfig: nil,
            models: [],
            modelProviders: [:],
            middlewareFeatures: [],
            extensions: [],
            contributions: [],
            maxActiveSessions: 4
        ))

        XCTAssertNil(model.selectedSessionID)
        XCTAssertNil(model.agentDraft)
        XCTAssertEqual(model.providerDraft?.model, "gpt-5.6-sol")

        model.saveProviderAsDefault()
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        guard case .registerProvider(_, let provider, let modelIDs, let reasoningEfforts) = try XCTUnwrap(requests.first) else {
            return XCTFail("Expected first-provider registration")
        }
        XCTAssertEqual(provider, model.providerDraft)
        XCTAssertTrue(modelIDs.isEmpty)
        XCTAssertTrue(reasoningEfforts.isEmpty)

        let defaultConfig = VersionedAgentConfig(revision: 1, config: composition())
        model.applyGatewayCatalog(ready(defaultConfig: defaultConfig))
        XCTAssertNil(model.agentDraft)
        XCTAssertEqual(model.defaultAgentDraft, defaultConfig.config)
    }

    func testProviderSelectionUsesGatewayManifestDefaults() throws {
        let model = try model()
        model.defaultAgentDraft = AgentComposition(
            provider: ProviderConfig(
                provider: "old",
                model: "old-model",
                baseUrl: nil,
                reasoningEffort: nil,
                webSearch: .live
            ),
            middleware: MiddlewareConfig(
                enabled: [],
                settings: [
                    "context_offloading": ["stale_after_tokens": .integer(50_000)]
                ]
            ),
            extensions: [],
            systemPrompt: "Test",
            maxModelSteps: 256
        )
        model.providerStatuses = [ProviderStatus(
            provider: "kimi",
            label: "Kimi",
            symbol: "kimi",
            description: "Kimi Chat Completions API",
            configured: true,
            selection: nil,
            auth: .apiKey,
            defaultBaseUrl: nil,
            defaultApiKeyEnv: "MOONSHOT_API_KEY",
            models: [
                ProviderModel(
                    id: "kimi-k3",
                    label: "Kimi K3",
                    description: "Agentic coding model",
                    contextWindow: 1_048_576,
                    reasoning: [ReasoningChoice(
                        id: "max",
                        label: "Maximum",
                        description: "Maximum reasoning"
                    )],
                    defaultReasoning: "max"
                ),
                ProviderModel(
                    id: "kimi-k2.7-code",
                    label: "Kimi K2.7 Code",
                    description: "Coding model",
                    contextWindow: 262_144,
                    reasoning: [],
                    defaultReasoning: nil
                )
            ],
            modelIds: [],
            reasoningEfforts: [],
            modelIdsConfigurable: false,
            webSearch: [.off]
        )]

        model.selectProvider("kimi")

        XCTAssertEqual(model.defaultAgentDraft?.provider.model, "kimi-k3")
        XCTAssertEqual(model.defaultAgentDraft?.provider.reasoningEffort, "max")
        XCTAssertEqual(model.defaultAgentDraft?.provider.webSearch, .off)

        model.selectProviderModel("kimi-k2.7-code")

        XCTAssertEqual(model.defaultAgentDraft?.provider.model, "kimi-k2.7-code")
        XCTAssertNil(model.defaultAgentDraft?.provider.reasoningEffort)
    }

    func testConfigurableProviderCanonicalizesAndSavesModelAndReasoningCatalogs() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        let selection = ProviderConfig(
            provider: "responses",
            model: "old-model",
            baseUrl: "http://localhost:8080/v1",
            reasoningEffort: nil,
            webSearch: .off
        )
        model.defaultAgentDraft = composition()
        model.providerStatuses = [ProviderStatus(
            provider: selection.provider,
            label: "Local",
            symbol: "storage",
            description: "OpenAI-compatible endpoint",
            configured: true,
            selection: selection,
            auth: .apiKey,
            defaultBaseUrl: "http://localhost:8080/v1",
            defaultApiKeyEnv: nil,
            models: [],
            modelIds: ["old-model"],
            reasoningEfforts: ["medium"],
            modelIdsConfigurable: true,
            webSearch: [.off]
        )]
        model.selectProvider(selection.provider)
        model.updateProviderModelIDs(" model-a, model-b, , model-a ")
        model.updateProviderReasoningEfforts(" high, medium, , high ")

        XCTAssertEqual(model.providerModelIDs, ["model-a", "model-b"])
        XCTAssertEqual(model.providerReasoningEfforts, ["high", "medium"])
        model.saveProviderAsDefault()
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        let request = try XCTUnwrap(requests.first)
        guard case .registerProvider(
            _,
            let config,
            let modelIDs,
            let reasoningEfforts
        ) = request else {
            return XCTFail("Expected provider registration")
        }
        XCTAssertEqual(modelIDs, ["model-a", "model-b"])
        XCTAssertEqual(reasoningEfforts, ["high", "medium"])
        XCTAssertEqual(config.model, "model-a")
        XCTAssertEqual(config.reasoningEffort, "high")
    }

    func testScopedModelSelectionUsesGatewayProviderIdentity() throws {
        let model = try model()
        let target = ProviderConfig(
            provider: "kimi",
            model: "kimi-k3",
            baseUrl: nil,
            reasoningEffort: "max",
            webSearch: .off
        )
        let choice = ModelChoice(
            route: "opaque-route",
            group: "Kimi · K3",
            model: target.model,
            reasoningEffort: target.reasoningEffort,
            contextWindow: 1_048_576,
            supportsImageInput: true
        )
        let original = composition()
        model.agentSnapshot = VersionedAgentConfig(revision: 1, config: original)
        model.defaultAgentSnapshot = VersionedAgentConfig(revision: 1, config: original)
        model.agentDraft = original
        model.defaultAgentDraft = original
        model.modelChoices = [choice]
        model.modelProviders = [choice.route: target.provider]
        model.providerStatuses = [providerStatus(for: target)]

        model.selectAgentDraftModel(choice.route)

        XCTAssertEqual(model.agentDraft?.provider, target)
        XCTAssertEqual(model.agentDraftModelRoute, choice.route)
        XCTAssertEqual(model.defaultAgentDraft, original)
        XCTAssertNotEqual(model.agentDraft, model.agentSnapshot?.config)

        model.agentDraft = original
        model.selectDefaultAgentDraftModel(choice.route)

        XCTAssertEqual(model.defaultAgentDraft?.provider, target)
        XCTAssertEqual(model.defaultAgentDraftModelRoute, choice.route)
        XCTAssertEqual(model.agentDraft, original)
    }

    func testModelLabelUsesProviderFriendlyName() throws {
        let model = try model()
        let config = ProviderConfig(
            provider: "openai_socket",
            model: "gpt-5.6-sol",
            baseUrl: nil,
            reasoningEffort: "high",
            webSearch: .cached
        )
        let choice = ModelChoice(
            route: "opaque-route",
            group: "OpenAI · Sol",
            model: config.model,
            reasoningEffort: config.reasoningEffort,
            contextWindow: 128_000,
            supportsImageInput: true
        )
        model.modelProviders = [choice.route: config.provider]
        model.providerStatuses = [providerStatus(for: config, models: [ProviderModel(
            id: config.model,
            label: "Sol",
            description: "Coding model",
            contextWindow: 128_000,
            reasoning: [],
            defaultReasoning: "high"
        )])]

        XCTAssertEqual(model.modelLabel(for: choice), "Sol")
        XCTAssertEqual(model.modelLabel(
            for: ModelChoice(
                route: "custom-route",
                group: "Custom",
                model: "custom-model",
                reasoningEffort: nil,
                contextWindow: nil,
                supportsImageInput: false
            )
        ), "custom-model")
    }

    func testProviderLabelsUseAdvertisedNames() throws {
        let model = try model()
        let codex = ProviderConfig(
            provider: "openai_codex",
            model: "gpt-5.4",
            baseUrl: nil,
            reasoningEffort: "high",
            webSearch: .off
        )
        model.providerStatuses = [
            providerStatus(for: codex, label: "Codex"),
            providerStatus(for: ProviderConfig(
                provider: "openai_socket",
                model: "gpt-5.6-sol",
                baseUrl: nil,
                reasoningEffort: "high",
                webSearch: .cached
            ), label: "OpenAI"),
            providerStatus(for: ProviderConfig(
                provider: "responses",
                model: "local-model",
                baseUrl: "http://localhost:8080/v1",
                reasoningEffort: nil,
                webSearch: .off
            ), label: "Local")
        ]

        XCTAssertEqual(model.providerLabel(for: "openai_codex"), "Codex")
        XCTAssertEqual(model.providerLabel(for: "openai_socket"), "OpenAI")
        XCTAssertEqual(model.providerLabel(for: "responses"), "Local")
    }

    func testMiddlewareSettingsSetAndClearWithoutCapabilityLogic() {
        var middleware = MiddlewareConfig(enabled: ["example"], settings: [:])

        middleware.setSetting(.string("route-a"), middleware: "example", setting: "route")
        XCTAssertEqual(middleware.settings["example"]?["route"], .string("route-a"))

        middleware.setSetting(nil, middleware: "example", setting: "route")
        XCTAssertNil(middleware.settings["example"])
    }

    func testGatewayDefaultRefreshDoesNotOverwriteActiveAgentDraft() throws {
        let model = try model()
        let active = AgentComposition(
            provider: ProviderConfig(
                provider: "openai_socket",
                model: "gpt-5.6-sol",
                baseUrl: nil,
                reasoningEffort: "medium",
                webSearch: .cached
            ),
            middleware: MiddlewareConfig(
                enabled: ["extensions"],
                settings: [
                    "context_offloading": ["stale_after_tokens": .integer(50_000)]
                ]
            ),
            extensions: [],
            systemPrompt: "Active",
            maxModelSteps: 256
        )
        var edited = active
        edited.systemPrompt = "Unsaved active edit"
        var gatewayDefault = active
        gatewayDefault.systemPrompt = "New chat default"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: active)
        model.agentDraft = edited
        model.defaultAgentSnapshot = VersionedAgentConfig(revision: 7, config: active)
        model.defaultAgentDraft = active

        model.applyGatewayCatalog(ReadyPayload(
            machineName: "snowwhite.local",
            sessions: [],
            providers: [],
            defaultConfig: VersionedAgentConfig(revision: 8, config: gatewayDefault),
            models: [],
            modelProviders: [:],
            middlewareFeatures: [],
            extensions: [],
            contributions: [],
            maxActiveSessions: 4
        ))

        XCTAssertEqual(model.agentSnapshot, VersionedAgentConfig(revision: 3, config: active))
        XCTAssertEqual(model.agentDraft, edited)
        XCTAssertEqual(model.defaultAgentDraft, gatewayDefault)
        XCTAssertEqual(
            model.defaultAgentSnapshot,
            VersionedAgentConfig(revision: 8, config: gatewayDefault)
        )
    }

    func testClearingCurrentChatPreservesGatewayDefaultSettings() throws {
        let model = try model()
        let active = composition(systemPrompt: "Active chat")
        let gatewayDefault = composition(systemPrompt: "New chats")
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.sessions = [session(state: .idle)]
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: active)
        model.agentDraft = active
        model.chatAgentApplyState = .failed("Chat failure")
        model.defaultAgentSnapshot = VersionedAgentConfig(revision: 8, config: gatewayDefault)
        model.defaultAgentDraft = gatewayDefault
        model.defaultAgentApplyState = .failed("Default failure")

        model.applySessions([])

        XCTAssertNil(model.selectedSessionID)
        XCTAssertNil(model.agentSnapshot)
        XCTAssertNil(model.agentDraft)
        XCTAssertEqual(model.chatAgentApplyState, .idle)
        XCTAssertEqual(
            model.defaultAgentSnapshot,
            VersionedAgentConfig(revision: 8, config: gatewayDefault)
        )
        XCTAssertEqual(model.defaultAgentDraft, gatewayDefault)
        XCTAssertEqual(model.defaultAgentApplyState, .failed("Default failure"))
    }

    func testComposerSettingConfiguresTheActiveChatWithoutCapabilityLogic() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        var active = composition()
        active.middleware.setSetting(
            .string("safe"),
            middleware: "example",
            setting: "access"
        )
        model.selectedSessionID = "chat-1"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: active)
        model.agentDraft = active

        model.setAgentSettingForCurrentChat(
            .string("broader"),
            middleware: "example",
            setting: "access"
        )
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        let request = try XCTUnwrap(requests.first)
        guard case .configureSession(_, _, let expectedRevision, let config) = request else {
            return XCTFail("Expected composer setting to configure the active chat")
        }
        XCTAssertEqual(expectedRevision, 3)
        XCTAssertEqual(
            config.middleware.settings["example"]?["access"],
            .string("broader")
        )
    }

    func testProviderRegistrationChainsIntoDefaultConfiguration() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        let draft = composition(systemPrompt: "New default")
        let previousDefault = composition(systemPrompt: "Previous default")
        model.selectedSessionID = "chat-1"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: composition())
        model.agentDraft = composition(systemPrompt: "Active chat")
        model.defaultAgentSnapshot = VersionedAgentConfig(
            revision: 8,
            config: previousDefault
        )
        model.defaultAgentDraft = draft
        model.providerStatuses = [providerStatus(for: draft.provider)]

        model.saveProviderAsDefault()
        try await Task.sleep(for: .milliseconds(20))

        let registrationRequests = await recorder.requests()
        let registration = try XCTUnwrap(
            registrationRequests.lazy.compactMap { request -> String? in
                guard case .registerProvider(let requestID, _, _, _) = request else { return nil }
                return requestID
            }.first
        )
        let response = ready(defaultConfig: VersionedAgentConfig(
            revision: 8,
            config: previousDefault
        ))
        model.handle(.ready(response))
        XCTAssertEqual(model.defaultAgentDraft, draft)
        model.applyGatewayConfigurationResponse(requestID: registration, payload: response)
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        guard let configured = requests.first(where: {
            if case .configureDefaultAgent = $0 { return true }
            return false
        }), case .configureDefaultAgent(
            _,
            let expectedRevision,
            let config
        ) = configured else {
            return XCTFail("Expected provider registration to configure the gateway default")
        }
        XCTAssertEqual(expectedRevision, 8)
        XCTAssertEqual(config, draft)
    }

    func testSavingDefaultLeavesActiveChatUntouched() async throws {
        let recorder = GatewayRequestRecorder()
        let defaultSaved = expectation(description: "Default agent saved")
        let sessionConfigured = expectation(description: "Active chat configured")
        sessionConfigured.isInverted = true
        let model = try model { request in
            await recorder.record(request)
            if case .configureDefaultAgent = request { defaultSaved.fulfill() }
            if case .configureSession = request { sessionConfigured.fulfill() }
        }
        let active = composition()
        var draft = active
        draft.middleware.setSetting(
            .string("first"),
            middleware: "example",
            setting: "mode"
        )
        model.selectedSessionID = "chat-1"
        model.sessions = [session(state: .idle)]
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: active)
        model.defaultAgentSnapshot = VersionedAgentConfig(revision: 7, config: active)
        model.agentDraft = active
        model.defaultAgentDraft = draft

        model.saveAgentAsDefault()
        await fulfillment(of: [defaultSaved], timeout: 1)

        let defaultRequests = await recorder.requests()
        let defaultRequest = try XCTUnwrap(
            defaultRequests.first { request in
                if case .configureDefaultAgent = request { return true }
                return false
            }
        )
        guard case .configureDefaultAgent(let requestID, _, let savedDraft) = defaultRequest else {
            return XCTFail("Expected default-agent configuration")
        }
        XCTAssertEqual(savedDraft, draft)

        let response = ready(
            defaultConfig: VersionedAgentConfig(revision: 8, config: draft)
        )
        model.handle(.ready(response))
        XCTAssertEqual(model.defaultAgentDraft, draft)
        model.applyGatewayConfigurationResponse(requestID: requestID, payload: response)
        await fulfillment(of: [sessionConfigured], timeout: 0.05)

        let sessionRequests = await recorder.requests()
        XCTAssertFalse(sessionRequests.contains {
            if case .configureSession = $0 { return true }
            return false
        })
        XCTAssertEqual(model.agentDraft, active)
        XCTAssertEqual(model.defaultAgentDraft, draft)
        XCTAssertEqual(model.defaultAgentApplyState, .applied)
        XCTAssertEqual(model.chatAgentApplyState, .idle)
    }

    func testSavingDefaultPreservesALaterDefaultDraft() async throws {
        let recorder = GatewayRequestRecorder()
        let defaultSaved = expectation(description: "Default agent saved")
        let sessionConfigured = expectation(description: "Active chat configured")
        sessionConfigured.isInverted = true
        let model = try model { request in
            await recorder.record(request)
            if case .configureDefaultAgent = request { defaultSaved.fulfill() }
            if case .configureSession = request { sessionConfigured.fulfill() }
        }
        let active = composition()
        var draft = active
        draft.middleware.setSetting(
            .string("first"),
            middleware: "example",
            setting: "mode"
        )
        model.selectedSessionID = "chat-1"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: active)
        model.defaultAgentSnapshot = VersionedAgentConfig(revision: 7, config: active)
        model.agentDraft = active
        model.defaultAgentDraft = draft

        model.saveAgentAsDefault()
        await fulfillment(of: [defaultSaved], timeout: 1)
        let requests = await recorder.requests()
        let requestID = try XCTUnwrap(requests.lazy.compactMap { request -> String? in
            guard case .configureDefaultAgent(let requestID, _, _) = request else { return nil }
            return requestID
        }.first)

        var laterDefaultDraft = draft
        laterDefaultDraft.middleware.setSetting(
            .string("second"),
            middleware: "example",
            setting: "mode"
        )
        model.defaultAgentDraft = laterDefaultDraft
        let response = ready(
            defaultConfig: VersionedAgentConfig(revision: 8, config: draft)
        )
        model.handle(.ready(response))
        XCTAssertEqual(model.defaultAgentDraft, laterDefaultDraft)
        model.applyGatewayConfigurationResponse(requestID: requestID, payload: response)
        await fulfillment(of: [sessionConfigured], timeout: 0.05)

        let configuredSessions = await recorder.requests().filter { request in
            if case .configureSession = request { return true }
            return false
        }
        XCTAssertTrue(configuredSessions.isEmpty)
        XCTAssertEqual(model.agentDraft, active)
        XCTAssertEqual(model.defaultAgentDraft, laterDefaultDraft)
        XCTAssertEqual(model.defaultAgentApplyState, .applied)
        XCTAssertEqual(model.chatAgentApplyState, .idle)
    }

    func testInstallingAnExtensionUsesTheGatewayRefreshAsItsResult() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        model.connectionState = .ready

        model.extensionInstallSource = " https://github.com/DietrichGebert/ponytail.git "
        model.installExtension()

        let request = await recorder.firstRequest(after: 0) {
            if case .installExtension = $0 { return true }
            return false
        }
        guard case .installExtension(
            let requestID,
            let source,
            let reference,
            let subdirectory
        ) = try XCTUnwrap(request) else {
            return XCTFail("Expected an extension install request")
        }
        XCTAssertEqual(source, "https://github.com/DietrichGebert/ponytail.git")
        XCTAssertNil(reference)
        XCTAssertNil(subdirectory)
        XCTAssertEqual(model.extensionAction, .installing)

        let installed = extensionRecord()
        let response = ready(
            defaultConfig: VersionedAgentConfig(revision: 1, config: composition()),
            extensions: [installed]
        )
        model.applyGatewayConfigurationResponse(requestID: requestID, payload: response)

        XCTAssertEqual(model.extensions, [installed])
        XCTAssertNil(model.extensionAction)
        XCTAssertTrue(model.extensionInstallSource.isEmpty)
        XCTAssertEqual(model.toast?.message, "Extension installed.")
    }

    func testHookTrustChangesAreBoundToTheInstalledDigest() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        model.connectionState = .ready

        let untrusted = extensionRecord(hooksTrusted: false)
        model.trustHooks(for: untrusted)

        let trust = await recorder.firstRequest(after: 0) {
            if case .trustExtensionHooks = $0 { return true }
            return false
        }
        guard case .trustExtensionHooks(
            let trustRequestID,
            let trustID,
            let trustDigest
        ) = try XCTUnwrap(trust) else {
            return XCTFail("Expected a hook trust request")
        }
        XCTAssertEqual(trustID, untrusted.id)
        XCTAssertEqual(trustDigest, untrusted.digest)
        XCTAssertEqual(model.extensionAction, .trusting(untrusted.name))

        model.completeExtensionAction(requestID: trustRequestID)
        XCTAssertEqual(model.toast?.message, "\(untrusted.name) hooks trusted.")
        let trusted = extensionRecord()
        model.untrustHooks(for: trusted)

        let untrust = await recorder.firstRequest(after: 1) {
            if case .revokeExtensionHooksTrust = $0 { return true }
            return false
        }
        guard case .revokeExtensionHooksTrust(
            _,
            let untrustID,
            let untrustDigest
        ) = try XCTUnwrap(untrust) else {
            return XCTFail("Expected a hook trust revocation request")
        }
        XCTAssertEqual(untrustID, trusted.id)
        XCTAssertEqual(untrustDigest, trusted.digest)
        XCTAssertEqual(model.extensionAction, .untrusting(trusted.name))
    }

    func testCatalogRefreshPreservesStableMissingExtensionReferences() throws {
        let model = try model()
        let snapshot = VersionedAgentConfig(revision: 1, config: composition())
        var unsavedDefault = snapshot.config
        unsavedDefault.extensions = ["plugin:ponytail"]
        var unsavedChat = snapshot.config
        unsavedChat.extensions = ["plugin:ponytail"]
        model.defaultAgentSnapshot = snapshot
        model.defaultAgentDraft = unsavedDefault
        model.agentDraft = unsavedChat

        model.applyGatewayCatalog(ready(defaultConfig: snapshot))

        XCTAssertEqual(model.defaultAgentDraft?.extensions, ["plugin:ponytail"])
        XCTAssertEqual(model.agentDraft?.extensions, ["plugin:ponytail"])
    }

    func testFatalGatewayErrorClearsAnExtensionAction() throws {
        let model = try model { _ in }
        model.connectionState = .ready
        model.extensionInstallSource = "https://github.com/DietrichGebert/ponytail.git"
        model.installExtension()
        XCTAssertEqual(model.extensionAction, .installing)

        model.handle(.error(GatewayFailure(
            code: "internal",
            message: "Gateway failed.",
            fatal: true
        )))

        XCTAssertNil(model.extensionAction)
        XCTAssertNil(model.extensionRequestID)
        XCTAssertEqual(
            model.extensionInstallSource,
            "https://github.com/DietrichGebert/ponytail.git"
        )
    }

}
