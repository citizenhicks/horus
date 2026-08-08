import Foundation
import Observation
import XCTest

private actor GatewayRequestRecorder {
    private var recorded: [GatewayRequest] = []

    func record(_ request: GatewayRequest) {
        recorded.append(request)
    }

    func requests() -> [GatewayRequest] {
        recorded
    }
}

private actor GatewayConnectionHarness {
    enum Failure: Error { case unavailable }

    private var attempts = 0
    private var continuation: AsyncThrowingStream<GatewayEnvelope, Error>.Continuation?

    func open(
        _ endpoint: GatewayEndpoint
    ) throws -> AsyncThrowingStream<GatewayEnvelope, Error> {
        _ = endpoint
        attempts += 1
        guard attempts > 1 else { throw Failure.unavailable }
        var continuation: AsyncThrowingStream<GatewayEnvelope, Error>.Continuation!
        let stream = AsyncThrowingStream<GatewayEnvelope, Error> { continuation = $0 }
        self.continuation = continuation
        return stream
    }

    func yield(_ envelope: GatewayEnvelope) {
        continuation?.yield(envelope)
    }

    func fail() {
        continuation?.finish(throwing: Failure.unavailable)
        continuation = nil
    }

    func attemptCount() -> Int { attempts }
}

@MainActor
final class AppModelTests: XCTestCase {
    private func model(
        requestSender: (@MainActor @Sendable (GatewayRequest) async throws -> Void)? = nil
    ) throws -> AppModel {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        return AppModel(
            client: GatewayClient(),
            store: GatewayStore(
                defaults: defaults,
                transcriptDirectory: directory,
                draftDirectory: directory.appendingPathComponent("Drafts", isDirectory: true)
            ),
            settingsDefaults: defaults,
            appLockAuthenticator: AppLockAuthenticator(
                method: { .unavailable },
                authenticate: { _ in false }
            ),
            requestSender: requestSender
        )
    }

    private func composition(systemPrompt: String = "Test") -> AgentComposition {
        AgentComposition(
            provider: ProviderConfig(
                provider: "openai_socket",
                model: "gpt-5.6-sol",
                baseUrl: nil,
                reasoningEffort: "high",
                webSearch: .cached
            ),
            middleware: MiddlewareConfig(
                enabled: ["skills", "subagents"],
                settings: [
                    "context_offloading": ["stale_after_tokens": .integer(50_000)],
                    "subagents": [
                        "model_route": .string("openai_socket::gpt-5.6-sol::high")
                    ]
                ]
            ),
            systemPrompt: systemPrompt
        )
    }

    private func providerStatus(for config: ProviderConfig) -> ProviderStatus {
        ProviderStatus(
            provider: config.provider,
            label: "OpenAI",
            symbol: "sparkle",
            description: "Test provider",
            configured: true,
            selection: config,
            auth: .apiKey,
            defaultBaseUrl: config.baseUrl,
            defaultApiKeyEnv: "OPENAI_API_KEY",
            models: [],
            modelIds: [],
            modelIdsConfigurable: false,
            webSearch: [config.webSearch]
        )
    }

    private func ready(defaultConfig: VersionedAgentConfig) -> ReadyPayload {
        ReadyPayload(
            machineName: "snowwhite.local",
            sessions: [session(state: .idle)],
            providers: [],
            defaultConfig: defaultConfig,
            models: [],
            modelProviders: [:],
            middlewareFeatures: [],
            maxActiveSessions: 4
        )
    }

    private func fileAttachmentContribution() -> FrontendContribution {
        FrontendContribution(
            capability: "files",
            acceptsFileAttachments: true,
            count: nil,
            commands: [],
            widgets: [],
            references: [],
            activeInput: nil
        )
    }

    private func sessionReady(
        latestSequence: UInt64,
        replayEpoch: String = "epoch-1",
        nextBeforeSequence: UInt64? = nil,
        sessionID: String = "chat-1",
        contributions: [FrontendContribution] = [],
        widgets: [SessionWidget] = [],
        runStats: RunStats = RunStats()
    ) -> SessionReadyPayload {
        SessionReadyPayload(
            replayEpoch: replayEpoch,
            latestSequence: latestSequence,
            nextBeforeSequence: nextBeforeSequence,
            workspace: WorkspaceInfo(id: "workspace-1", path: "/srv/horus"),
            git: nil,
            session: SessionConfigured(
                sessionId: sessionID,
                context: SessionContext(
                    tenantId: nil,
                    userId: nil,
                    userName: nil,
                    workspaceId: "workspace-1",
                    workspaceLabel: "/srv/horus",
                    originLabel: nil
                ),
                model: ModelChanged(
                    route: "openai",
                    model: "gpt-5.6-sol",
                    reasoningEffort: "high",
                    modelContextWindow: 200_000
                )
            ),
            contributions: contributions,
            widgets: widgets,
            toolCount: 0,
            runStats: runStats,
            config: VersionedAgentConfig(revision: 1, config: composition())
        )
    }

    private func renderEvent(
        capability: String = "tools",
        id: String = "result",
        group: String? = "turn",
        append: Bool = false,
        pending: Bool = false,
        text: String,
        format: String = "plain_text",
        tone: String = "neutral"
    ) -> AgentEventRecord {
        AgentEventRecord(submissionId: nil, msg: .object([
            "type": .string("frontend"),
            "frontendType": .string("render"),
            "capability": .string(capability),
            "block": .object([
                "id": .string(id),
                "group": group.map(JSONValue.string) ?? .null,
                "append": .bool(append),
                "pending": .bool(pending),
                "text": .string(text),
                "format": .string(format),
                "tone": .string(tone)
            ])
        ]))
    }

    private func session(
        sessionID: String = "chat-1",
        state: SessionActivityState,
        outcome: SessionOutcome? = nil,
        message: String? = nil,
        turnID: String? = nil,
        executionStats: ExecutionStats = ExecutionStats(),
        createdAt: Int64 = 100,
        updatedAt: Int64 = 100
    ) -> SessionRecord {
        SessionRecord(
            sessionId: sessionID,
            sessionContext: SessionContext(
                tenantId: nil,
                userId: nil,
                userName: nil,
                workspaceId: "workspace-1",
                workspaceLabel: "/srv/horus",
                originLabel: nil
            ),
            parentSessionId: nil,
            parentSequence: nil,
            sequence: 1,
            catalogVisible: true,
            firstUserMessage: "Review",
            executionStats: executionStats,
            title: nil,
            pinned: false,
            activity: SessionActivity(
                state: state,
                turnId: turnID,
                startedAt: state == .idle ? nil : 100,
                lastOutcome: outcome,
                message: message
            ),
            createdAt: createdAt,
            updatedAt: updatedAt
        )
    }

    func testSessionElapsedTimesTheRunningTurn() throws {
        let model = try model()
        model.selectedSessionID = "chat-1"
        model.sessions = [session(state: .idle, createdAt: 100, updatedAt: 160)]

        XCTAssertEqual(model.sessionElapsed(at: Date(timeIntervalSince1970: 200)), 0)

        // `session(state:)` starts a running turn at 100, not at the chat's creation time.
        model.sessions = [session(state: .running, createdAt: 20, updatedAt: 160)]
        XCTAssertEqual(model.sessionElapsed(at: Date(timeIntervalSince1970: 200)), 100)
    }

    func testLiveRunStatsStartImmediatelyAndTrackToolCalls() throws {
        let model = try model()
        model.selectedSessionID = "chat-1"

        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("task_started"),
                "turnId": .string("turn-1")
            ])),
            blocks: [],
            preview: nil
        )
        let active = try XCTUnwrap(model.runStats.active)
        XCTAssertEqual(model.sessionRunCount, 1)
        XCTAssertGreaterThan(
            model.sessionElapsed(at: Date(timeIntervalSince1970: TimeInterval(active.startedAtMs) / 1_000 + 2)),
            1.9
        )

        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("tool_call_begin")
            ])),
            blocks: [],
            preview: nil
        )
        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("tool_call_end"),
                "isError": .bool(true)
            ])),
            blocks: [],
            preview: nil
        )
        XCTAssertEqual(model.sessionToolCalls, 1)
        XCTAssertEqual(model.sessionFailedToolCalls, 1)

        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("task_complete")
            ])),
            blocks: [],
            preview: nil
        )
        XCTAssertNil(model.runStats.active)
    }

    func testStreamDeltasStayBatchedUntilTheCanonicalMessage() async throws {
        let model = try model()

        for _ in 0..<100 {
            model.reduce(
                event: AgentEventRecord(submissionId: nil, msg: .object([
                    "type": .string("agent_message_content_delta"),
                    "itemId": .string("answer-1"),
                    "delta": .string("x")
                ])),
                blocks: [],
                preview: nil
            )
        }
        XCTAssertTrue(model.transcript.isEmpty)

        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(model.transcript.map(\.text), [String(repeating: "x", count: 100)])
        XCTAssertTrue(try XCTUnwrap(model.transcript.first).pending)

        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("agent_message"),
                "message": .string("Canonical **Markdown**")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.transcript.map(\.text), ["Canonical **Markdown**"])
        XCTAssertFalse(try XCTUnwrap(model.transcript.first).pending)
    }

    func testTaskCompleteFlushesPendingReasoning() throws {
        let model = try model()

        for delta in ["think", "ing"] {
            model.reduce(
                event: AgentEventRecord(submissionId: nil, msg: .object([
                    "type": .string("agent_reasoning_content_delta"),
                    "itemId": .string("reasoning-1"),
                    "delta": .string(delta)
                ])),
                blocks: [],
                preview: nil
            )
        }
        XCTAssertTrue(model.transcript.isEmpty)

        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("task_complete")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.transcript.map(\.text), ["thinking"])
        XCTAssertFalse(try XCTUnwrap(model.transcript.first).pending)
    }

    func testAppLockAuthenticatesBeforePersistingAndRelocks() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        var method = AppLockAuthenticationMethod.unavailable
        var results = [false, true, true]
        let authenticator = AppLockAuthenticator(
            method: { method },
            authenticate: { _ in results.removeFirst() }
        )
        let app = AppModel(
            client: GatewayClient(),
            store: GatewayStore(defaults: defaults),
            settingsDefaults: defaults,
            appLockAuthenticator: authenticator
        )
        await app.appDidBecomeActive()

        await app.setAppLockEnabled(true)
        XCTAssertFalse(app.appLockEnabled)
        XCTAssertFalse(defaults.bool(forKey: "app-lock-enabled"))

        method = .faceID
        await app.setAppLockEnabled(true)
        XCTAssertFalse(app.appLockEnabled)
        XCTAssertEqual(app.appLockAuthenticationMethod.settingTitle, "Require Face ID")

        await app.setAppLockEnabled(true)
        XCTAssertTrue(app.appLockEnabled)
        XCTAssertFalse(app.isAppLocked)
        XCTAssertTrue(defaults.bool(forKey: "app-lock-enabled"))

        let relaunched = AppModel(
            client: GatewayClient(),
            store: GatewayStore(defaults: defaults),
            settingsDefaults: defaults,
            appLockAuthenticator: authenticator
        )
        XCTAssertTrue(relaunched.isAppLocked)
        await relaunched.appDidBecomeActive()
        XCTAssertFalse(relaunched.isAppLocked)
        relaunched.textFilePreview = TextFilePreview(id: UUID(), name: "secret.swift", contents: "secret")
        relaunched.appDidEnterBackground()
        XCTAssertTrue(relaunched.isAppLocked)
        XCTAssertNil(relaunched.textFilePreview)
        XCTAssertTrue(results.isEmpty)
    }

    func testThemeUsesTheInjectedDefaults() throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        defaults.set(ThemePreference.dark.rawValue, forKey: "theme")
        let model = AppModel(
            client: GatewayClient(),
            store: GatewayStore(defaults: defaults),
            settingsDefaults: defaults
        )

        XCTAssertEqual(model.theme, .dark)
        model.setTheme(.light)
        XCTAssertEqual(defaults.string(forKey: "theme"), ThemePreference.light.rawValue)
    }

    func testGitBranchSwitchUsesAnAdvertisedBranch() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.gitStatus = GitStatus(currentBranch: "main", branches: ["feature", "main"])

        model.switchGitBranch(to: "unknown")
        model.switchGitBranch(to: "feature")
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        XCTAssertEqual(requests.count, 1)
        guard case .switchGitBranch(_, let sessionID, let branch) = requests[0] else {
            return XCTFail("Expected a branch switch request")
        }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(branch, "feature")
    }

    func testSteeringDraftSettlesOnSuccessAndRestoresOnWarning() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.selectedSessionID = "chat-1"
        model.activeTurnID = "turn-1"
        model.activeOperation = "steer"
        model.composer = "Use the smaller patch"

        model.sendMessage()
        try await Task.sleep(for: .milliseconds(20))
        let firstSubmissions = await recorder.requests().compactMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        }
        let first = try XCTUnwrap(firstSubmissions.first)
        model.reduce(
            event: AgentEventRecord(submissionId: first.id, msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("remove_widget"),
                "capability": .string("steering"),
                "id": .string("queued")
            ])),
            blocks: [],
            preview: nil
        )
        model.handle(.rejected(GatewayRejection(
            requestId: "unrelated",
            code: "connection_failed",
            message: "Disconnected",
            fatal: true
        )))

        XCTAssertEqual(model.composer, "")

        model.composer = "Retry this steering"
        model.sendMessage()
        try await Task.sleep(for: .milliseconds(20))
        let secondSubmissions = await recorder.requests().compactMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        }
        let second = try XCTUnwrap(secondSubmissions.last)
        model.reduce(
            event: AgentEventRecord(submissionId: second.id, msg: .object([
                "type": .string("warning"),
                "message": .string("Steering queue is full")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.composer, "Retry this steering")
    }

    func testSwitchingGatewaysClearsGatewayScopedStateBeforeTokenLookup() throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let store = GatewayStore(defaults: defaults)
        let first = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        let second = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9192"))
        store.select(first)

        let model = AppModel(client: GatewayClient(), store: store)
        model.accounts = [first, second]
        model.connectionState = .ready
        model.composer = "Gateway A draft"
        model.providerAPIKey = "gateway-a-secret"
        model.providerActionState = .credentialSaved("Gateway A")
        model.pairingCodeInfo = PairingCodeInfo(code: "1234", expiresAt: .distantFuture)

        model.selectAccount(second.id)

        XCTAssertEqual(model.selectedAccountID, second.id)
        XCTAssertEqual(model.connectionState, .connecting)
        XCTAssertEqual(model.composer, "")
        XCTAssertEqual(model.providerAPIKey, "")
        XCTAssertEqual(model.providerActionState, .idle)
        XCTAssertNil(model.pairingCodeInfo)
    }

    func testRenamingGatewayPersistsItsFriendlyName() throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let account = GatewayAccount(
            endpoint: try GatewayEndpoint("wss://gateway.example"),
            displayName: "Gateway"
        )
        defaults.set(try JSONEncoder().encode([account]), forKey: "paired-gateways")
        defaults.set(account.id.uuidString, forKey: "selected-gateway")
        let store = GatewayStore(defaults: defaults)
        let model = AppModel(client: GatewayClient(), store: store)

        model.renameSelectedGateway("Home gateway")

        XCTAssertEqual(model.selectedAccount?.displayName, "Home gateway")
        XCTAssertEqual(store.loadAccounts().first?.displayName, "Home gateway")
    }

    func testReactivationReplacesAStaleConnectionAndPreservesTheActiveChat() throws {
        let model = try model()
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.selectedSessionID = "chat-1"
        model.connectionState = .ready

        model.setSceneActive(true)
        XCTAssertEqual(model.connectionState, .ready)

        model.setSceneActive(false)
        model.setSceneActive(true)

        XCTAssertEqual(model.connectionState, .connecting)
        XCTAssertEqual(model.selectedSessionID, "chat-1")
    }

    func testAutomaticReconnectRestoresDraftWithoutReplayingSubmission() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer {
            defaults.removePersistentDomain(forName: suiteName)
            try? FileManager.default.removeItem(at: root)
        }
        let store = GatewayStore(
            defaults: defaults,
            transcriptDirectory: root.appendingPathComponent("Transcripts", isDirectory: true),
            draftDirectory: root.appendingPathComponent("Drafts", isDirectory: true)
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try store.save(account, token: "test-token")
        addTeardownBlock { try await store.remove(account) }
        let harness = GatewayConnectionHarness()
        let recorder = GatewayRequestRecorder()
        let model = AppModel(
            client: GatewayClient(),
            store: store,
            settingsDefaults: defaults,
            requestSender: { request in await recorder.record(request) },
            connectionOpener: { endpoint in try await harness.open(endpoint) },
            reconnectDelay: { _ in .zero }
        )
        await model.appDidBecomeActive()

        model.start()
        try await Task.sleep(for: .milliseconds(100))
        let connectedAttempts = await harness.attemptCount()
        XCTAssertEqual(connectedAttempts, 2)
        await harness.yield(.authenticated)
        await harness.yield(.ready(ready(
            defaultConfig: VersionedAgentConfig(revision: 1, config: composition())
        )))
        try await Task.sleep(for: .milliseconds(50))
        model.selectedSessionID = "chat-1"
        model.connectionState = .ready
        model.composer = "Run this once"
        model.sendMessage()
        try await Task.sleep(for: .milliseconds(30))

        await harness.fail()
        try await Task.sleep(for: .milliseconds(100))

        let submissions = await recorder.requests().filter { request in
            if case .submit = request { return true }
            return false
        }
        XCTAssertEqual(submissions.count, 1)
        XCTAssertEqual(model.composer, "Run this once")
        let reconnectAttempts = await harness.attemptCount()
        XCTAssertEqual(reconnectAttempts, 3)
    }

    func testApprovalRemainsAvailableWhenSendFails() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let model = AppModel(
            client: GatewayClient(),
            store: GatewayStore(defaults: defaults)
        )
        let approval = PendingApproval(
            id: "approval-1",
            reason: "Run the command?",
            calls: [ApprovalCall(id: "call-1", name: "shell", arguments: "{}")]
        )
        model.selectedSessionID = "chat-1"
        model.pendingApproval = approval

        model.resolveApproval(.approved)
        try await Task.sleep(for: .milliseconds(20))

        XCTAssertEqual(model.pendingApproval, approval)
        XCTAssertEqual(model.toast?.tone, .error)
    }

    func testFrontendRenderIsNamespacedAndReplayedFromHistory() throws {
        let app = try model()
        let first = renderEvent(
            pending: true,
            text: "Started",
            tone: "warning"
        )
        app.reduce(event: first, blocks: [], preview: nil)
        app.reduce(
            event: renderEvent(group: nil, append: true, text: " and finished", tone: "success"),
            blocks: [],
            preview: nil
        )

        let entry = try XCTUnwrap(app.transcript.first)
        XCTAssertEqual(entry.id, "tools/result")
        XCTAssertEqual(entry.group, "tools/turn")
        XCTAssertEqual(entry.text, "Started and finished")
        XCTAssertEqual(entry.tone, "success")
        XCTAssertFalse(entry.pending)

        let replay = try model()
        replay.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("session_history"),
                "events": .array([])
            ])),
            blocks: [],
            history: [RenderedEventRecord(event: first.msg, blocks: [])],
            preview: nil
        )
        XCTAssertEqual(replay.transcript.first?.id, "tools/result")
        XCTAssertEqual(replay.transcript.first?.tone, "warning")
    }

    func testPreviewPreservesRenderedBlocksAndCapabilityRender() throws {
        let model = try model()
        let outer = FrontendBlock(
            id: "tools/call",
            group: "tools/turn",
            append: false,
            pending: false,
            text: "Read file",
            format: "plain_text",
            tone: "neutral"
        )
        let rendered = renderEvent(
            capability: "reviewer",
            id: "change",
            group: "work",
            text: "@@ -1 +1 @@",
            format: "unified_diff",
            tone: "success"
        )
        let preview = RenderedPreview(title: "worker", events: [
            RenderedEventRecord(event: .object(["type": .string("tool_call_end")]), blocks: [outer]),
            RenderedEventRecord(event: rendered.msg, blocks: [])
        ])

        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("preview"),
                "title": .string("worker"),
                "events": .array([])
            ])),
            blocks: [],
            preview: preview
        )

        let snapshot = try XCTUnwrap(model.previews.first)
        XCTAssertEqual(snapshot.title, "worker")
        XCTAssertEqual(snapshot.blocks.map(\.block.text), ["Read file", "@@ -1 +1 @@"])
        XCTAssertEqual(snapshot.blocks.last?.block.id, "reviewer/change")
        XCTAssertEqual(snapshot.blocks.last?.block.group, "reviewer/work")
        XCTAssertEqual(snapshot.blocks.last?.block.format, "unified_diff")
        XCTAssertEqual(snapshot.blocks.last?.block.tone, "success")
        XCTAssertNil(model.presentedPreview)
        XCTAssertFalse(model.showsInspector)
    }

    func testSelectedPickerPreviewPresentsOneTranscriptWithAgentMetadata() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.selectedSessionID = "chat-1"
        model.submitPickerOption(try FrontendPickerOption(json: .object([
            "label": .string("reviewer"),
            "description": .string("running"),
            "detail": .string("gpt-5.6-sol"),
            "op": .object([
                "type": .string("capability_command"),
                "capability": .string("subagents"),
                "command": .string("subagents"),
                "arguments": .string("reviewer"),
                "input": .null,
                "target": .null
            ])
        ])))
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        let submission = try XCTUnwrap(requests.lazy.compactMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        }.first)
        let block = FrontendBlock(
            id: "worker/message",
            group: nil,
            append: false,
            pending: false,
            text: "Done",
            format: "plain_text",
            tone: "success"
        )
        model.reduce(
            event: AgentEventRecord(submissionId: submission.id, msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("preview"),
                "title": .string("reviewer"),
                "events": .array([])
            ])),
            blocks: [],
            preview: RenderedPreview(title: "reviewer", events: [
                RenderedEventRecord(event: .object(["type": .string("agent_message")]), blocks: [block])
            ])
        )

        XCTAssertEqual(model.presentedPreview?.title, "reviewer")
        XCTAssertEqual(model.presentedPreview?.status, "running")
        XCTAssertEqual(model.presentedPreview?.model, "gpt-5.6-sol")
        XCTAssertEqual(model.presentedPreview?.blocks.map(\.block.text), ["Done"])
        XCTAssertFalse(model.showsInspector)
    }

    func testFrontendPickerUsesGenericPromptForAnyCapability() throws {
        let model = try model()

        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("picker"),
                "title": .string("Choose a review action"),
                "options": .array([.object([
                    "label": .string("Accept"),
                    "description": .string("Accept the review result."),
                    "detail": .string("reviewer-v1"),
                    "op": .object([
                        "type": .string("capability_command"),
                        "capability": .string("reviewer"),
                        "command": .string("accept"),
                        "arguments": .string(""),
                        "input": .null,
                        "target": .null
                    ])
                ])])
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.pendingPicker?.title, "Choose a review action")
        XCTAssertEqual(model.pendingPicker?.options.first?.label, "Accept")
        XCTAssertFalse(model.showsInspector)
    }

    func testFrontendOperationSubmitsEditedCapabilityInput() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.selectedSessionID = "chat-1"

        model.submitFrontendOperation(.capabilityCommand(
            capability: "notes",
            command: "edit",
            arguments: "note-1",
            input: "Use one row.",
            target: nil
        ))
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        guard case .submit(let sessionID, let submission) = try XCTUnwrap(requests.first),
              case .capabilityCommand(
                  let capability,
                  let command,
                  let arguments,
                  let input,
                  let target
              ) = submission.op
        else { return XCTFail("Expected edited capability command") }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(capability, "notes")
        XCTAssertEqual(command, "edit")
        XCTAssertEqual(arguments, "note-1")
        XCTAssertEqual(input, "Use one row.")
        XCTAssertNil(target)
    }

    func testUnifiedDiffRefreshesGatewayChanges() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.selectedSessionID = "chat-1"
        model.connectionState = .ready

        model.reduce(
            event: renderEvent(
                capability: "reviewer",
                text: "@@ -1 +1 @@",
                format: "unified_diff"
            ),
            blocks: [],
            preview: nil
        )
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        XCTAssertTrue(requests.contains {
            guard case .getGitDiff(_, "chat-1", .unstaged) = $0 else { return false }
            return true
        })
    }

    func testDuplicateSessionIdentifiersAreRejectedWithoutReplacingTheCatalog() throws {
        let model = try model()
        let original = session(state: .idle)
        model.sessions = [original]

        model.applySessions([original, session(state: .running)])

        XCTAssertEqual(model.sessions, [original])
        XCTAssertEqual(model.toast?.tone, .error)
    }

    func testIdenticalSessionCatalogDoesNotPublishAChange() async throws {
        let model = try model()
        let catalog = [session(state: .idle)]
        model.applySessions(catalog)
        let changed = expectation(description: "sessions changed")
        changed.isInverted = true
        withObservationTracking {
            _ = model.sessions
        } onChange: {
            changed.fulfill()
        }

        model.applySessions(catalog)

        await fulfillment(of: [changed], timeout: 0.05)
    }

    func testAttachmentUploadUsesAcknowledgedChunksAndSendsNativeReferences() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.selectedModelRoute = "openai"
        let capableChoice = ModelChoice(
            route: "openai",
            group: "OpenAI",
            model: "gpt-5.6-sol",
            reasoningEffort: "high",
            contextWindow: 200_000,
            supportsImageInput: true
        )
        model.modelChoices = [capableChoice]
        let attachmentContribution = FrontendContribution(
            capability: "files",
            acceptsFileAttachments: true,
            count: nil,
            commands: [],
            widgets: [],
            references: [],
            activeInput: nil
        )
        model.contributions = [attachmentContribution]

        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let fileURL = directory.appendingPathComponent("scan.png")
        try Data([1, 2, 3]).write(to: fileURL)

        await model.importAttachments([fileURL])
        try await Task.sleep(for: .milliseconds(20))

        let initialRequests = await recorder.requests()
        guard let begin = initialRequests.first(where: {
            if case .beginAttachmentUpload = $0 { return true }
            return false
        }), case .beginAttachmentUpload(let beginID, let sessionID, let name, let size, _) = begin
        else { return XCTFail("Expected attachment upload start") }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(name, "scan.png")
        XCTAssertEqual(size, 3)

        model.handle(.attachmentUploadStarted(
            requestID: beginID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            maxChunkBytes: 2
        ))
        try await Task.sleep(for: .milliseconds(20))
        let afterStart = await recorder.requests()
        let firstChunk = try XCTUnwrap(afterStart.last(where: {
            if case .appendAttachmentChunk = $0 { return true }
            return false
        }))
        guard case .appendAttachmentChunk(let firstID, _, _, let firstOffset, let firstData) = firstChunk else {
            return XCTFail("Expected first attachment chunk")
        }
        XCTAssertEqual(firstOffset, 0)
        XCTAssertEqual(firstData, Data([1, 2]))

        model.handle(.attachmentChunkAccepted(
            requestID: firstID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            nextOffset: 2
        ))
        try await Task.sleep(for: .milliseconds(20))
        let afterFirstChunk = await recorder.requests()
        let secondChunk = try XCTUnwrap(afterFirstChunk.last(where: {
            if case .appendAttachmentChunk = $0 { return true }
            return false
        }))
        guard case .appendAttachmentChunk(let secondID, _, _, let secondOffset, let secondData) = secondChunk else {
            return XCTFail("Expected second attachment chunk")
        }
        XCTAssertEqual(secondOffset, 2)
        XCTAssertEqual(secondData, Data([3]))

        model.handle(.attachmentChunkAccepted(
            requestID: secondID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            nextOffset: 3
        ))
        try await Task.sleep(for: .milliseconds(20))
        let afterSecondChunk = await recorder.requests()
        let finish = try XCTUnwrap(afterSecondChunk.last(where: {
            if case .finishAttachmentUpload = $0 { return true }
            return false
        }))
        guard case .finishAttachmentUpload(let finishID, _, _) = finish else {
            return XCTFail("Expected attachment upload finish")
        }
        let attachment = AttachmentRecord(
            id: "file-1",
            name: "scan.png",
            size: 3,
            mediaType: "image/png"
        )
        model.handle(.attachmentUploaded(
            requestID: finishID,
            sessionID: "chat-1",
            attachment: attachment
        ))
        XCTAssertEqual(model.uploadedAttachments, [attachment])
        XCTAssertTrue(model.canSendComposer)

        model.contributions = []
        XCTAssertFalse(model.canSendComposer)
        model.sendMessage()
        XCTAssertEqual(model.toast?.message, "File attachments are not enabled for this chat.")
        model.contributions = [attachmentContribution]

        model.modelChoices = [ModelChoice(
            route: "openai",
            group: "OpenAI",
            model: "text-only",
            reasoningEffort: nil,
            contextWindow: 200_000,
            supportsImageInput: false
        )]
        XCTAssertTrue(model.canImportAttachments)
        XCTAssertFalse(model.canSendComposer)
        model.sendMessage()
        XCTAssertEqual(model.toast?.message, "The selected model does not accept image attachments.")
        model.modelChoices = [capableChoice]

        model.sendMessage()
        try await Task.sleep(for: .milliseconds(20))
        let afterSend = await recorder.requests()
        let submit = try XCTUnwrap(afterSend.last(where: {
            if case .submit = $0 { return true }
            return false
        }))
        guard case .submit(_, let submission) = submit,
              case .userInput(let text, let attachments) = submission.op
        else { return XCTFail("Expected attachment submission") }
        XCTAssertEqual(text, "")
        XCTAssertEqual(attachments, [attachment])

        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("user_message"),
                "message": .string(""),
                "attachments": .array([.object([
                    "id": .string("file-1"),
                    "name": .string("scan.png"),
                    "size": .number(3),
                    "mediaType": .string("image/png")
                ])]),
                "messageTarget": .null
            ])),
            blocks: [],
            preview: nil
        )
        XCTAssertEqual(model.transcript.last?.attachments, [attachment])
    }

    func testNonImageAttachmentSubmitsWithoutImageModelSupport() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.selectedModelRoute = "text-only"
        model.modelChoices = [ModelChoice(
            route: "text-only",
            group: "OpenAI",
            model: "text-only",
            reasoningEffort: nil,
            contextWindow: 200_000,
            supportsImageInput: false
        )]
        model.contributions = [fileAttachmentContribution()]
        let attachment = AttachmentRecord(
            id: "file-1",
            name: "notes.txt",
            size: 3,
            mediaType: "text/plain"
        )
        model.composerAttachments = [ComposerAttachment(
            id: UUID(),
            name: attachment.name,
            size: attachment.size,
            mediaType: attachment.mediaType,
            state: .uploaded(attachment)
        )]

        XCTAssertTrue(model.canImportAttachments)
        XCTAssertTrue(model.canSendComposer)
        model.sendMessage()
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        let submission = try XCTUnwrap(requests.compactMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        }.last)
        guard case .userInput(_, let attachments) = submission.op else {
            return XCTFail("Expected attachment submission")
        }
        XCTAssertEqual(attachments, [attachment])
    }

    func testAttachmentUploadRejectsKnownResponseWithWrongPhase() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.contributions = [fileAttachmentContribution()]
        let fileURL = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
            .appendingPathExtension("bin")
        try Data([1, 2, 3]).write(to: fileURL)
        defer { try? FileManager.default.removeItem(at: fileURL) }

        await model.importAttachments([fileURL])
        try await Task.sleep(for: .milliseconds(20))
        let requests = await recorder.requests()
        guard let begin = requests.last(where: {
            if case .beginAttachmentUpload = $0 { return true }
            return false
        }), case .beginAttachmentUpload(let beginID, _, _, _, _) = begin
        else { return XCTFail("Expected attachment upload start") }

        model.handle(.attachmentChunkAccepted(
            requestID: beginID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            nextOffset: 0
        ))

        let item = try XCTUnwrap(model.composerAttachments.first)
        guard case .failed(let message) = item.state else {
            return XCTFail("Expected invalid upload to fail")
        }
        XCTAssertEqual(message, "The gateway returned an invalid upload.")
    }

    func testAttachmentUploadRejectsUnexpectedAcknowledgedOffset() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.contributions = [fileAttachmentContribution()]
        let fileURL = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
            .appendingPathExtension("bin")
        try Data([1, 2, 3]).write(to: fileURL)
        defer { try? FileManager.default.removeItem(at: fileURL) }

        await model.importAttachments([fileURL])
        try await Task.sleep(for: .milliseconds(20))
        let initialRequests = await recorder.requests()
        guard let begin = initialRequests.last(where: {
            if case .beginAttachmentUpload = $0 { return true }
            return false
        }), case .beginAttachmentUpload(let beginID, _, _, _, _) = begin
        else { return XCTFail("Expected attachment upload start") }
        model.handle(.attachmentUploadStarted(
            requestID: beginID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            maxChunkBytes: 2
        ))
        try await Task.sleep(for: .milliseconds(20))
        let afterStart = await recorder.requests()
        guard let chunk = afterStart.last(where: {
            if case .appendAttachmentChunk = $0 { return true }
            return false
        }), case .appendAttachmentChunk(let chunkID, _, _, _, _) = chunk
        else { return XCTFail("Expected attachment chunk") }

        model.handle(.attachmentChunkAccepted(
            requestID: chunkID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            nextOffset: 1
        ))

        let item = try XCTUnwrap(model.composerAttachments.first)
        guard case .failed(let message) = item.state else {
            return XCTFail("Expected invalid offset to fail")
        }
        XCTAssertEqual(message, "The gateway returned an invalid upload offset.")
        let finalRequests = await recorder.requests()
        XCTAssertEqual(finalRequests.filter {
            if case .appendAttachmentChunk = $0 { return true }
            return false
        }.count, 1)
    }

    func testAttachmentMessageLimitIncludesUploadedFiles() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.selectedModelRoute = "openai"
        model.modelChoices = [ModelChoice(
            route: "openai",
            group: "OpenAI",
            model: "gpt-5.6-sol",
            reasoningEffort: "high",
            contextWindow: 200_000,
            supportsImageInput: true
        )]
        model.contributions = [FrontendContribution(
            capability: "files",
            acceptsFileAttachments: true,
            count: nil,
            commands: [],
            widgets: [],
            references: [],
            activeInput: nil
        )]
        let fileSize: Int64 = 25 * 1024 * 1024
        model.composerAttachments = (0..<4).map { index in
            let attachment = AttachmentRecord(
                id: "file-\(index)",
                name: "file-\(index).bin",
                size: fileSize,
                mediaType: "application/octet-stream"
            )
            return ComposerAttachment(
                id: UUID(),
                name: attachment.name,
                size: attachment.size,
                mediaType: attachment.mediaType,
                state: .uploaded(attachment)
            )
        }

        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let fileURL = directory.appendingPathComponent("extra.bin")
        try Data([1]).write(to: fileURL)

        await model.importAttachments([fileURL])

        XCTAssertEqual(model.composerAttachments.count, 4)
        let requests = await recorder.requests()
        XCTAssertTrue(requests.isEmpty)
        XCTAssertEqual(model.toast?.message, "Attachments in one message are limited to 100 MiB total.")
    }

    func testInspectorRequestsUnstagedDiffThenAllWorkspaceFiles() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"

        model.showInspector()
        try await Task.sleep(for: .milliseconds(20))
        let unstagedRequests = await recorder.requests()
        guard let unstagedRequest = unstagedRequests.last(where: {
            if case .getGitDiff = $0 { return true }
            return false
        }), case .getGitDiff(_, let sessionID, let diffScope) = unstagedRequest
        else { return XCTFail("Expected unstaged Git diff") }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(diffScope, .unstaged)
        XCTAssertFalse(unstagedRequests.contains { request in
            if case .listWorkspaceFiles = request { return true }
            return false
        })

        model.selectWorkspaceFileScope(.all)
        try await Task.sleep(for: .milliseconds(20))
        let allRequests = await recorder.requests()
        guard let allRequest = allRequests.last(where: {
            if case .listWorkspaceFiles = $0 { return true }
            return false
        }), case .listWorkspaceFiles(_, _, let allScope) = allRequest
        else { return XCTFail("Expected all workspace files") }
        XCTAssertEqual(allScope, .all)
    }

    func testWorkspaceSourceFileUsesTextPreview() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        let contents = "let answer = 42\n"
        let data = Data(contents.utf8)
        let file = WorkspaceFileRecord(path: "Sources/App.swift", size: UInt64(data.count))

        model.previewWorkspaceFile(file)
        try await Task.sleep(for: .milliseconds(20))
        let readRequests = await recorder.requests()
        guard let readRequest = readRequests.last(where: {
            if case .readWorkspaceFile = $0 { return true }
            return false
        }), case .readWorkspaceFile(let readID, _, let path, let offset, _) = readRequest
        else { return XCTFail("Expected workspace file read request") }
        XCTAssertEqual(path, file.path)
        XCTAssertEqual(offset, 0)
        model.handle(.workspaceFileChunk(
            requestID: readID,
            sessionID: "chat-1",
            path: file.path,
            offset: 0,
            data: data,
            nextOffset: nil
        ))
        try await Task.sleep(for: .milliseconds(20))

        XCTAssertEqual(model.textFilePreview?.contents, contents)
        XCTAssertNil(model.previewURL)
        model.discardAttachmentPreview()
    }

    func testWorkspaceBinaryFileUsesQuickLookPreview() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        let file = WorkspaceFileRecord(path: "image.bin", size: 3)

        model.previewWorkspaceFile(file)
        try await Task.sleep(for: .milliseconds(20))
        let requests = await recorder.requests()
        guard let request = requests.last(where: {
            if case .readWorkspaceFile = $0 { return true }
            return false
        }), case .readWorkspaceFile(let requestID, _, _, _, _) = request
        else { return XCTFail("Expected workspace file read request") }
        model.handle(.workspaceFileChunk(
            requestID: requestID,
            sessionID: "chat-1",
            path: file.path,
            offset: 0,
            data: Data([0, 1, 2]),
            nextOffset: nil
        ))
        try await Task.sleep(for: .milliseconds(20))

        XCTAssertNotNil(model.previewURL)
        XCTAssertNil(model.textFilePreview)
        model.discardAttachmentPreview()
    }

    func testContributionCatalogReferencesAndWidgetsAreGeneric() throws {
        let model = try model()
        model.contributions = [FrontendContribution(
            capability: "tasks",
            acceptsFileAttachments: false,
            count: 3,
            commands: [],
            widgets: [
                FrontendWidget(
                    id: "count",
                    slot: .header,
                    text: "3 tasks",
                    tone: "success",
                    symbol: nil,
                    iconOnly: false,
                    progress: nil,
                    content: nil,
                    action: nil
                ),
                FrontendWidget(
                    id: "fork",
                    slot: .messageActions,
                    text: "Fork chat",
                    tone: "neutral",
                    symbol: "branch",
                    iconOnly: true,
                    progress: nil,
                    content: nil,
                    action: .capabilityCommand(
                        capability: "sessions",
                        command: "fork",
                        arguments: "",
                        input: nil,
                        target: nil
                    )
                ),
                FrontendWidget(
                    id: "journal",
                    slot: .navigation,
                    text: "Journal",
                    tone: "neutral",
                    symbol: "brain",
                    iconOnly: false,
                    progress: nil,
                    content: nil,
                    action: nil
                ),
                FrontendWidget(
                    id: "journal-menu",
                    slot: .chatMenu,
                    text: "Open journal",
                    tone: "neutral",
                    symbol: "brain",
                    iconOnly: false,
                    progress: nil,
                    content: nil,
                    action: nil
                )
            ],
            references: [FrontendReference(trigger: "$", value: "planning", description: "Planning skill")],
            activeInput: nil
        )]
        model.middlewareFeatures = [MiddlewareFeature(
            id: "tasks",
            label: "Work items",
            description: "Tracks work items.",
            required: false,
            settings: []
        )]
        model.mountedWidgets = model.contributions.flatMap { contribution in
            contribution.widgets.map {
                MountedWidget(capability: contribution.capability, widget: $0)
            }
        }

        XCTAssertEqual(model.headerWidgets.first?.widget.text, "3 tasks")
        XCTAssertEqual(model.messageActionWidgets.first?.widget.text, "Fork chat")
        XCTAssertEqual(model.navigationWidgets.first?.id, "tasks\u{0}journal")
        XCTAssertEqual(model.chatMenuWidgets.first?.widget.text, "Open journal")
        XCTAssertEqual(model.middlewareContributionCounts, [MiddlewareContributionCount(
            id: "tasks",
            label: "Work items",
            value: 3
        )])
        let text = "Use $plan"
        let suggestions = try XCTUnwrap(model.referenceSuggestions(in: text, cursor: text.endIndex))
        XCTAssertEqual(String(text[suggestions.range]), "$plan")
        XCTAssertEqual(suggestions.matches.first?.replacement, "$planning")
    }

    func testSessionSnapshotKeepsStaticWidgetsAndUpsertsDynamicWidgets() throws {
        let model = try model()
        model.selectedSessionID = "chat-1"
        let staticStatus = FrontendWidget(
            id: "status",
            slot: .composerFooter,
            text: "Queued",
            tone: "neutral",
            symbol: "task",
            iconOnly: false,
            progress: nil,
            content: nil,
            action: nil
        )
        let navigation = FrontendWidget(
            id: "tasks",
            slot: .navigation,
            text: "Tasks",
            tone: "neutral",
            symbol: "task",
            iconOnly: false,
            progress: nil,
            content: nil,
            action: nil
        )
        let dynamicStatus = FrontendWidget(
            id: "status",
            slot: .composerFooter,
            text: "Running",
            tone: "warning",
            symbol: "task",
            iconOnly: false,
            progress: nil,
            content: nil,
            action: nil
        )
        let contribution = FrontendContribution(
            capability: "tasks",
            acceptsFileAttachments: false,
            count: 1,
            commands: [],
            widgets: [staticStatus, navigation],
            references: [],
            activeInput: nil
        )

        model.handle(.sessionChanged(sessionReady(
            latestSequence: 1,
            contributions: [contribution],
            widgets: [SessionWidget(capability: "tasks", item: dynamicStatus)]
        )))

        XCTAssertEqual(model.mountedWidgets.count, 2)
        XCTAssertEqual(model.composerFooterWidgets.first?.widget.text, "Running")
        XCTAssertEqual(model.navigationWidgets.first?.widget.text, "Tasks")
    }

    func testMessageActionSubmitsTheClickedHistoryTarget() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.selectedSessionID = "chat-1"
        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("session_history"),
                "events": .array([])
            ])),
            blocks: [],
            history: [RenderedEventRecord(event: .object([
                "type": .string("user_message"),
                "message": .string("Fork here"),
                "messageTarget": .object([
                    "checkpointSequence": .number(12),
                    "batchItemCount": .number(3)
                ])
            ]), blocks: [])],
            preview: nil
        )
        let target = try XCTUnwrap(model.transcript.first?.messageTarget)
        let widget = MountedWidget(
            capability: "sessions",
            widget: FrontendWidget(
                id: "fork",
                slot: .messageActions,
                text: "Fork chat",
                tone: "neutral",
                symbol: "arrow.triangle.branch",
                iconOnly: true,
                progress: nil,
                content: nil,
                action: .capabilityCommand(
                    capability: "sessions",
                    command: "fork",
                    arguments: "",
                    input: nil,
                    target: nil
                )
            )
        )

        model.submitMessageAction(widget, target: target)
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        guard case .submit(let sessionID, let submission) = try XCTUnwrap(requests.first),
              case .capabilityCommand(
                  let capability,
                  let command,
                  let arguments,
                  let input,
                  let submittedTarget
              ) = submission.op
        else { return XCTFail("Expected a targeted capability command") }
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(capability, "sessions")
        XCTAssertEqual(command, "fork")
        XCTAssertEqual(arguments, "")
        XCTAssertNil(input)
        XCTAssertEqual(submittedTarget, MessageTarget(checkpointSequence: 12, batchItemCount: 3))
    }

    func testWebSearchAbortAndLiveUsageMatchCLI() throws {
        let model = try model()
        for msg: JSONValue in [
            .object(["type": .string("web_search_begin"), "callId": .string("search-1")]),
            .object([
                "type": .string("web_search_end"),
                "callId": .string("search-1"),
                "query": .string("Horus"),
                "action": .string("search")
            ]),
            .object([
                "type": .string("turn_aborted"),
                "turnId": .string("turn-1"),
                "reason": .string("Stopped")
            ]),
            .object([
                "type": .string("token_count"),
                "info": .object([
                    "totalTokenUsage": .object([
                        "inputTokens": .number(1_000),
                        "cachedInputTokens": .number(100),
                        "cacheWriteInputTokens": .number(25),
                        "outputTokens": .number(100),
                        "reasoningOutputTokens": .number(50),
                        "totalTokens": .number(1_100)
                    ]),
                    "lastTokenUsage": .object([
                        "inputTokens": .number(40),
                        "cachedInputTokens": .number(20),
                        "cacheWriteInputTokens": .number(5),
                        "outputTokens": .number(10),
                        "reasoningOutputTokens": .number(3),
                        "totalTokens": .number(99)
                    ]),
                    "modelContextWindow": .number(200)
                ])
            ])
        ] {
            model.reduce(
                event: AgentEventRecord(submissionId: nil, msg: msg),
                blocks: [],
                preview: nil
            )
        }

        XCTAssertEqual(model.transcript.map(\.text), [
            "Searching the web",
            "Searched: Horus",
            "Turn aborted: Stopped"
        ])
        XCTAssertEqual(model.transcript.map(\.tone), ["warning", "success", "warning"])
        XCTAssertEqual(model.currentUsage.inputTokens, 1_000)
        XCTAssertEqual(model.lastUsage.cachedInputTokens, 20)
        XCTAssertEqual(model.lastUsage.cacheWriteInputTokens, 5)
        XCTAssertEqual(model.contextTokens, 99)
        XCTAssertEqual(model.modelContextWindow, 200)
    }

    func testSessionSnapshotsDriveActivityAndOnlyUnseenCompletion() throws {
        let model = try model()
        model.applySessions([session(state: .idle)])
        model.selectedSessionID = "chat-1"
        model.destination = .agent
        model.setChatVisible(false)

        model.applySessions([session(state: .running, turnID: "turn-1")])
        XCTAssertTrue(model.runningSessionIDs.contains("chat-1"))

        model.applySessions([session(state: .awaitingApproval, turnID: "turn-1")])
        XCTAssertEqual(model.toast?.tone, .warning)

        model.applySessions([session(state: .idle, outcome: .completed)])
        XCTAssertFalse(model.runningSessionIDs.contains("chat-1"))
        XCTAssertTrue(model.unreadSessionIDs.contains("chat-1"))
        XCTAssertEqual(model.toast?.tone, .success)

        model.destination = .chat
        model.setChatVisible(true)
        XCTAssertFalse(model.unreadSessionIDs.contains("chat-1"))
        model.dismissToast()
        model.applySessions([session(state: .running, turnID: "turn-2")])
        model.applySessions([session(state: .idle, outcome: .completed)])
        XCTAssertNil(model.toast)
    }

    func testFailedSessionSnapshotUsesGatewayMessage() throws {
        let model = try model()
        model.applySessions([session(state: .idle)])
        model.selectedSessionID = "chat-1"
        model.setChatVisible(false)

        model.applySessions([session(state: .running, turnID: "turn-1")])
        model.applySessions([session(
            state: .idle,
            outcome: .failed,
            message: "Provider failed"
        )])

        XCTAssertEqual(model.toast?.message, "Review failed: Provider failed.")
        XCTAssertEqual(model.toast?.tone, .error)
        XCTAssertTrue(model.unreadSessionIDs.contains("chat-1"))

        model.showToast("Credential saved.", tone: .success)
        XCTAssertEqual(model.toast?.message, "Credential saved.")
        XCTAssertEqual(model.toast?.tone, .success)
    }

    func testAgentEventsDoNotDriveCatalogActivityOrToasts() throws {
        let model = try model()
        model.applySessions([session(state: .idle)])
        model.selectedSessionID = "chat-1"

        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("task_started"),
                "turnId": .string("turn-1")
            ])),
            blocks: [],
            preview: nil
        )
        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("error"),
                "message": .string("Provider failed")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertFalse(model.runningSessionIDs.contains("chat-1"))
        XCTAssertTrue(model.unreadSessionIDs.isEmpty)
        XCTAssertNil(model.toast)
        XCTAssertEqual(model.transcript.last?.text, "Provider failed")
    }

    func testSetupValidationUsesGlobalToast() throws {
        let model = try model()
        model.pairingEndpoint = "tcp://localhost:9191"

        model.pair()

        XCTAssertEqual(model.toast?.message, "Enter the one-time code shown by the gateway.")
        XCTAssertEqual(model.toast?.tone, .error)

        model.dismissToast()
        model.saveProviderCredential(provider: "openai")

        XCTAssertEqual(model.toast?.message, "Enter an API key. It will be sent once and never read back.")
        XCTAssertEqual(model.toast?.tone, .error)
    }

    func testPairingSetupPrefillsWithoutPairing() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        model.showsPairing = false
        model.pairingError = "Old error"

        model.applyPairingSetup(
            "horus-pair:v1|wss://gateway.example|0123456789abcdef"
        )

        XCTAssertTrue(model.showsPairing)
        XCTAssertEqual(model.pairingEndpoint, "wss://gateway.example")
        XCTAssertEqual(model.pairingCode, "0123456789abcdef")
        XCTAssertNil(model.pairingError)
        let requests = await recorder.requests()
        XCTAssertTrue(requests.isEmpty)
    }

    func testPairingURLPrefillsWithoutPairing() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }

        model.applyPairingURL(try XCTUnwrap(URL(string:
            "horus://pair?endpoint=wss%3A%2F%2Fgateway.example&code=0123456789abcdef"
        )))

        XCTAssertTrue(model.showsPairing)
        XCTAssertEqual(model.pairingEndpoint, "wss://gateway.example")
        XCTAssertEqual(model.pairingCode, "0123456789abcdef")
        XCTAssertNil(model.pairingError)
        let requests = await recorder.requests()
        XCTAssertTrue(requests.isEmpty)
    }

    func testEmptyGatewayCanRegisterItsFirstProviderWithoutAChat() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        model.applyGatewayCatalog(ReadyPayload(
            machineName: "snowwhite.local",
            sessions: [],
            providers: [ProviderStatus(
                provider: "openai_socket",
                label: "OpenAI",
                symbol: "sparkle",
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
                modelIdsConfigurable: false,
                webSearch: [.off, .cached, .live]
            )],
            defaultConfig: nil,
            models: [],
            modelProviders: [:],
            middlewareFeatures: [],
            maxActiveSessions: 4
        ))

        XCTAssertNil(model.selectedSessionID)
        XCTAssertNil(model.agentDraft)
        XCTAssertEqual(model.providerDraft?.model, "gpt-5.6-sol")

        model.saveProviderAsDefault()
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        guard case .registerProvider(_, let provider, let modelIDs) = try XCTUnwrap(requests.first) else {
            return XCTFail("Expected first-provider registration")
        }
        XCTAssertEqual(provider, model.providerDraft)
        XCTAssertTrue(modelIDs.isEmpty)

        let defaultConfig = VersionedAgentConfig(revision: 1, config: composition())
        model.applyGatewayCatalog(ready(defaultConfig: defaultConfig))
        XCTAssertEqual(model.agentDraft, defaultConfig.config)
    }

    func testProviderSelectionUsesGatewayManifestDefaults() throws {
        let model = try model()
        model.agentDraft = AgentComposition(
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
            systemPrompt: "Test"
        )
        model.providerStatuses = [ProviderStatus(
            provider: "kimi",
            label: "Kimi",
            symbol: "moon",
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
            modelIdsConfigurable: false,
            webSearch: [.off]
        )]

        model.selectProvider("kimi")

        XCTAssertEqual(model.agentDraft?.provider.model, "kimi-k3")
        XCTAssertEqual(model.agentDraft?.provider.reasoningEffort, "max")
        XCTAssertEqual(model.agentDraft?.provider.webSearch, .off)

        model.selectProviderModel("kimi-k2.7-code")

        XCTAssertEqual(model.agentDraft?.provider.model, "kimi-k2.7-code")
        XCTAssertNil(model.agentDraft?.provider.reasoningEffort)
    }

    func testConfigurableProviderCanonicalizesAndSavesMultipleModelIDs() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        let selection = ProviderConfig(
            provider: "responses",
            model: "old-model",
            baseUrl: "http://localhost:8080/v1",
            reasoningEffort: nil,
            webSearch: .off
        )
        model.agentDraft = composition()
        model.providerStatuses = [ProviderStatus(
            provider: selection.provider,
            label: "Local and Other",
            symbol: "storage",
            description: "OpenAI-compatible endpoint",
            configured: true,
            selection: selection,
            auth: .apiKey,
            defaultBaseUrl: "http://localhost:8080/v1",
            defaultApiKeyEnv: nil,
            models: [],
            modelIds: ["old-model"],
            modelIdsConfigurable: true,
            webSearch: [.off]
        )]
        model.selectProvider(selection.provider)
        model.updateProviderModelIDs(" model-a, model-b, , model-a ")

        XCTAssertEqual(model.providerModelIDs, ["model-a", "model-b"])
        model.saveProviderAsDefault()
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        let request = try XCTUnwrap(requests.first)
        guard case .registerProvider(_, let config, let modelIDs) = request else {
            return XCTFail("Expected provider registration")
        }
        XCTAssertEqual(modelIDs, ["model-a", "model-b"])
        XCTAssertEqual(config.model, "model-a")
    }

    func testDefaultModelSelectionUsesGatewayProviderIdentity() throws {
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
        model.modelChoices = [choice]
        model.modelProviders = [choice.route: target.provider]
        model.providerStatuses = [providerStatus(for: target)]

        model.selectAgentDraftModel(choice.route)

        XCTAssertEqual(model.agentDraft?.provider, target)
        XCTAssertEqual(model.agentDraftModelRoute, choice.route)
        XCTAssertNotEqual(model.agentDraft, model.agentSnapshot?.config)
        XCTAssertNotEqual(model.agentDraft, model.defaultAgentSnapshot?.config)
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
                enabled: ["skills"],
                settings: [
                    "context_offloading": ["stale_after_tokens": .integer(50_000)]
                ]
            ),
            systemPrompt: "Active"
        )
        var edited = active
        edited.systemPrompt = "Unsaved active edit"
        var gatewayDefault = active
        gatewayDefault.systemPrompt = "New chat default"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: active)
        model.agentDraft = edited

        model.applyGatewayCatalog(ReadyPayload(
            machineName: "snowwhite.local",
            sessions: [],
            providers: [],
            defaultConfig: VersionedAgentConfig(revision: 8, config: gatewayDefault),
            models: [],
            modelProviders: [:],
            middlewareFeatures: [],
            maxActiveSessions: 4
        ))

        XCTAssertEqual(model.agentSnapshot, VersionedAgentConfig(revision: 3, config: active))
        XCTAssertEqual(model.agentDraft, edited)
        XCTAssertEqual(
            model.defaultAgentSnapshot,
            VersionedAgentConfig(revision: 8, config: gatewayDefault)
        )
    }

    func testApprovalPolicyConfiguresTheActiveChatThroughMiddleware() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        var active = composition()
        active.middleware.setSetting(
            .string("ask"),
            middleware: "sandbox",
            setting: "approval_policy"
        )
        model.selectedSessionID = "chat-1"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: active)
        model.agentDraft = active

        model.setApprovalPolicyForCurrentChat("allow_network")
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        let request = try XCTUnwrap(requests.first)
        guard case .configureSession(_, _, let expectedRevision, let config) = request else {
            return XCTFail("Expected approval policy to configure the active chat")
        }
        XCTAssertEqual(expectedRevision, 3)
        XCTAssertEqual(
            config.middleware.settings["sandbox"]?["approval_policy"],
            .string("allow_network")
        )
    }

    func testProviderRegistrationChainsIntoActiveChatConfiguration() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        let draft = composition(systemPrompt: "Active draft")
        model.selectedSessionID = "chat-1"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: composition())
        model.agentDraft = draft
        model.providerStatuses = [providerStatus(for: draft.provider)]

        model.changeProviderForCurrentChat()
        try await Task.sleep(for: .milliseconds(20))

        let registrationRequests = await recorder.requests()
        let registration = try XCTUnwrap(
            registrationRequests.lazy.compactMap { request -> String? in
                guard case .registerProvider(let requestID, _, _) = request else { return nil }
                return requestID
            }.first
        )
        model.applyGatewayConfigurationResponse(
            requestID: registration,
            payload: ready(defaultConfig: VersionedAgentConfig(
                revision: 8,
                config: composition(systemPrompt: "Gateway default")
            ))
        )
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        guard let configured = requests.first(where: {
            if case .configureSession = $0 { return true }
            return false
        }), case .configureSession(
            _,
            let sessionID,
            let expectedRevision,
            let config
        ) = configured else {
            return XCTFail("Expected provider registration to configure the active chat")
        }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(expectedRevision, 3)
        XCTAssertEqual(config, draft)
    }

    func testProviderRegistrationChainsIntoDefaultConfiguration() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        let draft = composition(systemPrompt: "New default")
        model.selectedSessionID = "chat-1"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: composition())
        model.agentDraft = draft
        model.providerStatuses = [providerStatus(for: draft.provider)]

        model.saveProviderAsDefault()
        try await Task.sleep(for: .milliseconds(20))

        let registrationRequests = await recorder.requests()
        let registration = try XCTUnwrap(
            registrationRequests.lazy.compactMap { request -> String? in
                guard case .registerProvider(let requestID, _, _) = request else { return nil }
                return requestID
            }.first
        )
        model.applyGatewayConfigurationResponse(
            requestID: registration,
            payload: ready(defaultConfig: VersionedAgentConfig(
                revision: 8,
                config: composition(systemPrompt: "Previous default")
            ))
        )
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

    func testSavingDefaultAlsoConfiguresActiveChat() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
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
        model.agentDraft = draft

        model.saveAgentAsDefault()
        try await Task.sleep(for: .milliseconds(20))

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

        model.applyGatewayConfigurationResponse(
            requestID: requestID,
            payload: ready(defaultConfig: VersionedAgentConfig(revision: 8, config: draft))
        )
        try await Task.sleep(for: .milliseconds(20))

        let sessionRequests = await recorder.requests()
        let sessionRequest = try XCTUnwrap(
            sessionRequests.first { request in
                if case .configureSession = request { return true }
                return false
            }
        )
        guard case .configureSession(
            _,
            let sessionID,
            let expectedRevision,
            let activeDraft
        ) = sessionRequest else {
            return XCTFail("Expected active-chat configuration")
        }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(expectedRevision, 3)
        XCTAssertEqual(activeDraft, draft)
    }

    func testSavingDefaultDoesNotApplyALaterDraftToActiveChat() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
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
        model.agentDraft = draft

        model.saveAgentAsDefault()
        try await Task.sleep(for: .milliseconds(20))
        let requests = await recorder.requests()
        let requestID = try XCTUnwrap(requests.lazy.compactMap { request -> String? in
            guard case .configureDefaultAgent(let requestID, _, _) = request else { return nil }
            return requestID
        }.first)

        var laterDraft = draft
        laterDraft.middleware.setSetting(
            .string("second"),
            middleware: "example",
            setting: "mode"
        )
        model.agentDraft = laterDraft
        model.applyGatewayConfigurationResponse(
            requestID: requestID,
            payload: ready(defaultConfig: VersionedAgentConfig(revision: 8, config: draft))
        )
        try await Task.sleep(for: .milliseconds(20))

        let configuredSessions = await recorder.requests().filter { request in
            if case .configureSession = request { return true }
            return false
        }
        XCTAssertTrue(configuredSessions.isEmpty)
        XCTAssertEqual(model.agentDraft, laterDraft)
        XCTAssertEqual(model.applyState, .applied)
    }

    func testHistoricalReplayAppearsOnlyWhenTheSnapshotIsComplete() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        model.openSession("chat-1")
        try await Task.sleep(for: .milliseconds(20))
        let requests = await recorder.requests()
        let request = try XCTUnwrap(requests.first)
        guard case .openSession(let requestID, _, nil, nil) = request else {
            return XCTFail("Expected an uncached session open")
        }
        model.handle(.sessionOpened(requestID: requestID, payload: sessionReady(latestSequence: 2)))
        model.showInspector()
        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 1,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("agent_message_content_delta"),
                "itemId": .string("answer-1"),
                "delta": .string("Hel")
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))
        XCTAssertTrue(model.displayedTranscript.isEmpty)

        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 2,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("agent_message"),
                "message": .string("Hello")
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertTrue(model.displayedTranscript.isEmpty)
        model.handle(.sessionReplayComplete(requestID: requestID, sessionID: "chat-1"))
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Hello"])
        try await Task.sleep(for: .milliseconds(20))
        let refreshed = await recorder.requests()
        XCTAssertTrue(refreshed.contains { request in
            guard case .getGitDiff(_, "chat-1", .unstaged) = request else { return false }
            return true
        })
    }

    func testEarlierHistoryUsesTheReadyCursorAndPrependsOnlyTranscriptState() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        model.openSession("chat-1")
        try await Task.sleep(for: .milliseconds(30))
        let openRequests = await recorder.requests()
        guard case .openSession(let openID, _, _, _) = try XCTUnwrap(
            openRequests.first
        ) else { return XCTFail("Expected session open") }
        model.handle(.sessionOpened(
            requestID: openID,
            payload: sessionReady(latestSequence: 8, nextBeforeSequence: 40)
        ))
        model.handle(.sessionReplayComplete(requestID: openID, sessionID: "chat-1"))
        model.transcript = [TranscriptEntry(
            id: "current",
            text: "Current",
            kind: .assistant,
            format: "plain_text",
            pending: false
        )]
        model.selectedModelRoute = "current-route"

        model.loadEarlierHistory()
        try await Task.sleep(for: .milliseconds(30))
        let requests = await recorder.requests()
        guard case .getSessionHistory(
            let historyID,
            "chat-1",
            40,
            20
        ) = try XCTUnwrap(requests.last(where: {
            if case .getSessionHistory = $0 { return true }
            return false
        })) else { return XCTFail("Expected paged history request") }

        let events = [
            RenderedEventRecord(event: .object([
                "type": .string("user_message"),
                "message": .string("Older question")
            ]), blocks: []),
            RenderedEventRecord(event: .object([
                "type": .string("model_changed"),
                "route": .string("historical-route")
            ]), blocks: []),
            RenderedEventRecord(event: .object([
                "type": .string("agent_message"),
                "message": .string("Older answer")
            ]), blocks: []),
        ]
        model.handle(.sessionHistory(
            requestID: "stale",
            sessionID: "chat-1",
            events: events,
            nextBeforeSequence: nil
        ))
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Current"])

        model.handle(.sessionHistory(
            requestID: historyID,
            sessionID: "chat-1",
            events: events,
            nextBeforeSequence: nil
        ))

        XCTAssertEqual(
            model.displayedTranscript.map(\.text),
            ["Older question", "Older answer", "Current"]
        )
        XCTAssertEqual(model.selectedModelRoute, "current-route")
        XCTAssertFalse(model.hasEarlierHistory)

        model.restoreSession("chat-1")
        try await Task.sleep(for: .milliseconds(30))
        let reconnectRequests = await recorder.requests()
        guard case .openSession(let reconnectID, _, _, _) = try XCTUnwrap(
            reconnectRequests.last
        ) else { return XCTFail("Expected reconnect session open") }
        model.handle(.sessionOpened(
            requestID: reconnectID,
            payload: sessionReady(latestSequence: 8, nextBeforeSequence: 40)
        ))
        model.handle(.sessionReplayComplete(requestID: reconnectID, sessionID: "chat-1"))
        XCTAssertFalse(model.hasEarlierHistory)
    }

    func testTranscriptStartsWithABoundedVisibleTail() throws {
        let model = try model()
        model.transcript = (0..<301).map { index in
            TranscriptEntry(
                id: "entry-\(index)",
                text: "\(index)",
                kind: .assistant,
                format: "plain_text",
                pending: false
            )
        }
        model.connectionState = .ready

        XCTAssertEqual(model.displayedTranscript.count, 300)
        XCTAssertEqual(model.displayedTranscript.first?.text, "1")
        XCTAssertTrue(model.hasEarlierHistory)

        model.loadEarlierHistory()

        XCTAssertEqual(model.displayedTranscript.count, 301)
        XCTAssertFalse(model.hasEarlierHistory)
    }

    func testCachedTranscriptSuppliesTheOpenCursorAndRestoresOnce() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer {
            defaults.removePersistentDomain(forName: suiteName)
            try? FileManager.default.removeItem(at: directory)
        }
        let recorder = GatewayRequestRecorder()
        let store = GatewayStore(defaults: defaults, transcriptDirectory: directory)
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        var currentUsage = TokenUsage()
        currentUsage.totalTokens = 55
        var lastUsage = TokenUsage()
        lastUsage.inputTokens = 30
        lastUsage.outputTokens = 12
        await store.saveTranscript(
            accountID: account.id,
            sessionID: "chat-1",
            replayEpoch: "epoch-1",
            sequence: 7,
            nextBeforeSequence: 40,
            transcript: [TranscriptEntry(
                id: "answer-1",
                text: "Already rendered",
                kind: .assistant,
                format: "plain_text",
                pending: false
            )],
            currentUsage: currentUsage,
            lastUsage: lastUsage
        )
        let model = AppModel(
            client: GatewayClient(),
            store: store,
            requestSender: { request in await recorder.record(request) }
        )
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        model.openSession("chat-1")
        try await Task.sleep(for: .milliseconds(20))
        let requests = await recorder.requests()
        let request = try XCTUnwrap(requests.first)
        guard case .openSession(let requestID, let sessionID, let cursor, let epoch) = request else {
            return XCTFail("Expected a cached session open")
        }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(cursor, 7)
        XCTAssertEqual(epoch, "epoch-1")

        model.handle(.sessionOpened(requestID: requestID, payload: sessionReady(latestSequence: 7)))
        XCTAssertEqual(model.transcript.map(\.text), ["Already rendered"])
        XCTAssertEqual(model.currentUsage.totalTokens, 55)
        XCTAssertEqual(model.contextTokens, 42)
        XCTAssertTrue(model.hasEarlierHistory)
        model.handle(.sessionReplayComplete(requestID: requestID, sessionID: "chat-1"))

        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 7,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("error"),
                "message": .string("Duplicate")
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))
        XCTAssertEqual(model.transcript.map(\.text), ["Already rendered"])
    }

    func testAuthoritativeHistoryReplacesTheFrozenCachedTranscript() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer {
            defaults.removePersistentDomain(forName: suiteName)
            try? FileManager.default.removeItem(at: directory)
        }
        let recorder = GatewayRequestRecorder()
        let store = GatewayStore(defaults: defaults, transcriptDirectory: directory)
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        await store.saveTranscript(
            accountID: account.id,
            sessionID: "chat-1",
            replayEpoch: "epoch-1",
            sequence: 7,
            transcript: [TranscriptEntry(
                id: "answer-1",
                text: "Cached",
                kind: .assistant,
                format: "plain_text",
                pending: false
            )],
            currentUsage: TokenUsage(),
            lastUsage: TokenUsage()
        )
        let model = AppModel(
            client: GatewayClient(),
            store: store,
            requestSender: { request in await recorder.record(request) }
        )
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        model.openSession("chat-1")
        try await Task.sleep(for: .milliseconds(20))
        let requests = await recorder.requests()
        guard case .openSession(let requestID, _, _, _) = try XCTUnwrap(requests.first) else {
            return XCTFail("Expected a session open")
        }
        model.handle(.sessionOpened(
            requestID: requestID,
            payload: sessionReady(latestSequence: 9)
        ))
        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 8,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("agent_message_content_delta"),
                "itemId": .string("answer-1"),
                "delta": .string(" updated")
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Cached"])
        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 9,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("session_history"),
                "events": .array([])
            ])),
            blocks: [],
            history: [RenderedEventRecord(event: .object([
                "type": .string("user_message"),
                "message": .string("Canonical")
            ]), blocks: [])],
            preview: nil
        ))
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Cached"])

        model.handle(.sessionReplayComplete(requestID: requestID, sessionID: "chat-1"))

        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Canonical"])
    }

    func testCursorRestoreKeepsTheReplayBaseAndFrozenPresentation() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer {
            defaults.removePersistentDomain(forName: suiteName)
            try? FileManager.default.removeItem(at: directory)
        }
        let recorder = GatewayRequestRecorder()
        let store = GatewayStore(defaults: defaults, transcriptDirectory: directory)
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        await store.saveTranscript(
            accountID: account.id,
            sessionID: "chat-1",
            replayEpoch: "epoch-1",
            sequence: 7,
            transcript: [TranscriptEntry(
                id: "answer-1",
                text: "Cached",
                kind: .assistant,
                format: "plain_text",
                pending: false
            )],
            currentUsage: TokenUsage(),
            lastUsage: TokenUsage()
        )
        let model = AppModel(
            client: GatewayClient(),
            store: store,
            requestSender: { request in await recorder.record(request) }
        )
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        model.openSession("chat-1")
        try await Task.sleep(for: .milliseconds(20))
        var requests = await recorder.requests()
        guard case .openSession(let firstRequestID, _, _, _) = try XCTUnwrap(requests.first) else {
            return XCTFail("Expected the first session open")
        }
        model.handle(.sessionOpened(
            requestID: firstRequestID,
            payload: sessionReady(latestSequence: 9)
        ))
        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 8,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("agent_message_content_delta"),
                "itemId": .string("answer-1"),
                "delta": .string(" updated")
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))

        model.restoreSession("chat-1")
        try await Task.sleep(for: .milliseconds(20))
        requests = await recorder.requests()
        guard case .openSession(let secondRequestID, _, 8, "epoch-1") = try XCTUnwrap(
            requests.last
        ) else { return XCTFail("Expected the replay cursor to resume at sequence 8") }
        model.handle(.sessionOpened(
            requestID: secondRequestID,
            payload: sessionReady(latestSequence: 9)
        ))
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Cached"])
        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 9,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("agent_message_content_delta"),
                "itemId": .string("answer-1"),
                "delta": .string(" again")
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))
        model.handle(.sessionReplayComplete(
            requestID: secondRequestID,
            sessionID: "chat-1"
        ))

        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Cached updated again"])
    }

    func testTranscriptCacheIsProtectedAndEvictsByRecency() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer {
            defaults.removePersistentDomain(forName: suiteName)
            try? FileManager.default.removeItem(at: directory)
        }
        let store = GatewayStore(defaults: defaults, transcriptDirectory: directory)
        let accountID = UUID()
        for index in 0..<20 {
            await store.saveTranscript(
                accountID: accountID,
                sessionID: "chat-\(index)",
                replayEpoch: "epoch-1",
                sequence: UInt64(index),
                transcript: [TranscriptEntry(
                    id: "answer-1",
                    text: "Cached",
                    kind: .assistant,
                    format: "plain_text",
                    pending: false
                )],
                currentUsage: TokenUsage(),
                lastUsage: TokenUsage()
            )
        }
        let accountDirectory = directory.appendingPathComponent(
            accountID.uuidString,
            isDirectory: true
        )
        for file in try FileManager.default.contentsOfDirectory(
            at: accountDirectory,
            includingPropertiesForKeys: nil
        ) {
            let cached = try JSONDecoder().decode(
                CachedTranscript.self,
                from: Data(contentsOf: file)
            )
            let date = cached.sequence == 0
                ? Date().addingTimeInterval(3600)
                : Date(timeIntervalSinceReferenceDate: TimeInterval(cached.sequence))
            try FileManager.default.setAttributes(
                [.modificationDate: date],
                ofItemAtPath: file.path
            )
        }
        await store.saveTranscript(
            accountID: accountID,
            sessionID: "chat-20",
            replayEpoch: "epoch-1",
            sequence: 20,
            transcript: [TranscriptEntry(
                id: "answer-1",
                text: "Cached",
                kind: .assistant,
                format: "plain_text",
                pending: false
            )],
            currentUsage: TokenUsage(),
            lastUsage: TokenUsage()
        )
        let files = try FileManager.default.contentsOfDirectory(
            at: accountDirectory,
            includingPropertiesForKeys: nil
        )
        XCTAssertEqual(files.count, 20)
        let newestOldCache = await store.loadTranscript(
            accountID: accountID,
            sessionID: "chat-0"
        )
        let oldestCache = await store.loadTranscript(
            accountID: accountID,
            sessionID: "chat-1"
        )
        let newCache = await store.loadTranscript(
            accountID: accountID,
            sessionID: "chat-20"
        )
        XCTAssertNotNil(newestOldCache)
        XCTAssertNil(oldestCache)
        XCTAssertNotNil(newCache)
        #if os(iOS) && !targetEnvironment(simulator)
        let attributes = try FileManager.default.attributesOfItem(atPath: XCTUnwrap(files.first).path)
        XCTAssertEqual(attributes[.protectionKey] as? FileProtectionType, .complete)
        #endif

        let oversizedAccountID = UUID()
        await store.saveTranscript(
            accountID: oversizedAccountID,
            sessionID: "large",
            replayEpoch: "epoch-1",
            sequence: 1,
            transcript: [TranscriptEntry(
                id: "answer-1",
                text: String(repeating: "x", count: 3 * 1024 * 1024 + 1),
                kind: .assistant,
                format: "plain_text",
                pending: false
            )],
            currentUsage: TokenUsage(),
            lastUsage: TokenUsage()
        )
        let oversized = await store.loadTranscript(
            accountID: oversizedAccountID,
            sessionID: "large"
        )
        XCTAssertNil(oversized)

        await store.saveTranscript(
            accountID: oversizedAccountID,
            sessionID: "corrupt",
            replayEpoch: "epoch-1",
            sequence: 1,
            transcript: [TranscriptEntry(
                id: "answer-1",
                text: "Cached",
                kind: .assistant,
                format: "plain_text",
                pending: false
            )],
            currentUsage: TokenUsage(),
            lastUsage: TokenUsage()
        )
        let oversizedDirectory = directory.appendingPathComponent(
            oversizedAccountID.uuidString,
            isDirectory: true
        )
        let oversizedURL = try XCTUnwrap(
            FileManager.default.contentsOfDirectory(
                at: oversizedDirectory,
                includingPropertiesForKeys: nil
            ).first
        )
        try Data(count: 4 * 1024 * 1024 + 1).write(to: oversizedURL, options: .atomic)
        let corrupt = await store.loadTranscript(
            accountID: oversizedAccountID,
            sessionID: "corrupt"
        )
        XCTAssertNil(corrupt)
        XCTAssertFalse(FileManager.default.fileExists(atPath: oversizedURL.path))
    }

    func testSwitchingSessionsFlushesAndRestoresTextDrafts() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer {
            defaults.removePersistentDomain(forName: suiteName)
            try? FileManager.default.removeItem(at: root)
        }
        let store = GatewayStore(
            defaults: defaults,
            transcriptDirectory: root.appendingPathComponent("Transcripts", isDirectory: true),
            draftDirectory: root.appendingPathComponent("Drafts", isDirectory: true)
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        await store.saveComposerDraft("Draft two", accountID: account.id, sessionID: "chat-2")
        let recorder = GatewayRequestRecorder()
        let model = AppModel(
            client: GatewayClient(),
            store: store,
            settingsDefaults: defaults,
            requestSender: { request in await recorder.record(request) }
        )
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        model.openSession("chat-1")
        try await Task.sleep(for: .milliseconds(30))
        let firstRequests = await recorder.requests()
        guard case .openSession(let firstID, "chat-1", _, _) = try XCTUnwrap(
            firstRequests.last
        ) else { return XCTFail("Expected first session open") }
        model.composer = "Typed while opening"
        model.handle(.sessionOpened(
            requestID: firstID,
            payload: sessionReady(latestSequence: 1, sessionID: "chat-1")
        ))
        model.handle(.sessionReplayComplete(requestID: firstID, sessionID: "chat-1"))
        try await Task.sleep(for: .milliseconds(30))
        XCTAssertEqual(model.composer, "Typed while opening")
        model.composer = "Draft one"

        model.openSession("chat-2")
        try await Task.sleep(for: .milliseconds(50))
        let secondRequests = await recorder.requests()
        let secondOpen = try XCTUnwrap(secondRequests.last(where: { request in
            guard case .openSession(_, "chat-2", _, _) = request else { return false }
            return true
        }))
        guard case .openSession(let secondID, _, _, _) = secondOpen else {
            return XCTFail("Expected second session open")
        }
        model.handle(.sessionOpened(
            requestID: secondID,
            payload: sessionReady(latestSequence: 1, sessionID: "chat-2")
        ))
        model.handle(.sessionReplayComplete(requestID: secondID, sessionID: "chat-2"))
        try await Task.sleep(for: .milliseconds(100))

        let firstDraft = await store.loadComposerDraft(
            accountID: account.id,
            sessionID: "chat-1"
        )
        XCTAssertEqual(firstDraft, "Draft one")
        XCTAssertEqual(model.composer, "Draft two")

        model.sendMessage()
        model.composer = "Next draft"
        try await Task.sleep(for: .milliseconds(50))
        let submittedDraft = await store.loadComposerDraft(
            accountID: account.id,
            sessionID: "chat-2"
        )
        XCTAssertEqual(submittedDraft, "Draft two")
        let submitted = await recorder.requests().compactMap { request -> String? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission.id
        }
        model.handle(.accepted(requestID: try XCTUnwrap(submitted.last)))
        try await Task.sleep(for: .milliseconds(50))
        let nextDraft = await store.loadComposerDraft(
            accountID: account.id,
            sessionID: "chat-2"
        )
        XCTAssertEqual(nextDraft, "Next draft")

        model.deleteSession(session(sessionID: "chat-2", state: .idle))
        try await Task.sleep(for: .milliseconds(30))
        let deleteRequests = await recorder.requests()
        guard case .deleteSession(let deleteID, "chat-2") = try XCTUnwrap(
            deleteRequests.last
        ) else { return XCTFail("Expected session delete") }
        model.handle(.accepted(requestID: deleteID))
        model.handle(.sessions(requestID: deleteID, sessions: []))
        try await Task.sleep(for: .milliseconds(50))
        let deletedDraft = await store.loadComposerDraft(
            accountID: account.id,
            sessionID: "chat-2"
        )
        XCTAssertTrue(model.composer.isEmpty)
        XCTAssertTrue(deletedDraft.isEmpty)
    }

    func testComposerDraftsAreDurableScopedAndBounded() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let transcriptDirectory = root.appendingPathComponent("Transcripts", isDirectory: true)
        let draftDirectory = root.appendingPathComponent("Drafts", isDirectory: true)
        defer {
            defaults.removePersistentDomain(forName: suiteName)
            try? FileManager.default.removeItem(at: root)
        }
        let firstAccount = GatewayAccount(
            endpoint: try GatewayEndpoint("tcp://localhost:9191")
        )
        let secondAccount = GatewayAccount(
            endpoint: try GatewayEndpoint("tcp://localhost:9192")
        )
        var store = GatewayStore(
            defaults: defaults,
            transcriptDirectory: transcriptDirectory,
            draftDirectory: draftDirectory
        )
        await store.saveComposerDraft(
            "First account",
            accountID: firstAccount.id,
            sessionID: "chat-1"
        )
        await store.saveComposerDraft(
            "Second account",
            accountID: secondAccount.id,
            sessionID: "chat-1"
        )

        store = GatewayStore(
            defaults: defaults,
            transcriptDirectory: transcriptDirectory,
            draftDirectory: draftDirectory
        )
        let restoredFirst = await store.loadComposerDraft(
            accountID: firstAccount.id,
            sessionID: "chat-1"
        )
        let restoredSecond = await store.loadComposerDraft(
            accountID: secondAccount.id,
            sessionID: "chat-1"
        )
        XCTAssertEqual(restoredFirst, "First account")
        XCTAssertEqual(restoredSecond, "Second account")

        await store.saveComposerDraft(
            "",
            accountID: firstAccount.id,
            sessionID: "chat-1"
        )
        await store.saveComposerDraft(
            "Existing",
            accountID: firstAccount.id,
            sessionID: "oversized"
        )
        await store.saveComposerDraft(
            String(repeating: "x", count: maximumComposerBytes + 1),
            accountID: firstAccount.id,
            sessionID: "oversized"
        )
        let removedEmpty = await store.loadComposerDraft(
            accountID: firstAccount.id,
            sessionID: "chat-1"
        )
        let removedOversized = await store.loadComposerDraft(
            accountID: firstAccount.id,
            sessionID: "oversized"
        )
        XCTAssertEqual(removedEmpty, "")
        XCTAssertEqual(removedOversized, "")

        await store.saveComposerDraft(
            "Will corrupt",
            accountID: firstAccount.id,
            sessionID: "corrupt"
        )
        let corruptFilename = Data("corrupt".utf8).base64EncodedString()
        let corruptURL = draftDirectory
            .appendingPathComponent(firstAccount.id.uuidString, isDirectory: true)
            .appendingPathComponent(corruptFilename)
            .appendingPathExtension("txt")
        try Data([0xFF]).write(to: corruptURL, options: .atomic)
        let corrupt = await store.loadComposerDraft(
            accountID: firstAccount.id,
            sessionID: "corrupt"
        )
        XCTAssertEqual(corrupt, "")
        XCTAssertFalse(FileManager.default.fileExists(atPath: corruptURL.path))

        await store.saveComposerDraft(
            "Remove with account",
            accountID: firstAccount.id,
            sessionID: "chat-2"
        )
        try await store.remove(firstAccount)
        let removedAccountDraft = await store.loadComposerDraft(
            accountID: firstAccount.id,
            sessionID: "chat-2"
        )
        let preservedAccountDraft = await store.loadComposerDraft(
            accountID: secondAccount.id,
            sessionID: "chat-1"
        )
        XCTAssertEqual(removedAccountDraft, "")
        XCTAssertEqual(preservedAccountDraft, "Second account")
    }

    func testUnavailableCachedCursorRetriesTheOpenWithoutIt() async throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer {
            defaults.removePersistentDomain(forName: suiteName)
            try? FileManager.default.removeItem(at: directory)
        }
        let recorder = GatewayRequestRecorder()
        let store = GatewayStore(defaults: defaults, transcriptDirectory: directory)
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        await store.saveTranscript(
            accountID: account.id,
            sessionID: "chat-1",
            replayEpoch: "epoch-1",
            sequence: 7,
            transcript: [TranscriptEntry(
                id: "answer-1",
                text: "Cached",
                kind: .assistant,
                format: "plain_text",
                pending: false
            )],
            currentUsage: TokenUsage(),
            lastUsage: TokenUsage()
        )
        let model = AppModel(
            client: GatewayClient(),
            store: store,
            requestSender: { request in await recorder.record(request) }
        )
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        model.openSession("chat-1")
        try await Task.sleep(for: .milliseconds(20))
        let requests = await recorder.requests()
        let first = try XCTUnwrap(requests.first)
        guard case .openSession(let requestID, _, 7, "epoch-1") = first else {
            return XCTFail("Expected the cached cursor")
        }
        model.handle(.rejected(GatewayRejection(
            requestId: requestID,
            code: "replay_unavailable",
            message: "Reload",
            fatal: false
        )))
        try await Task.sleep(for: .milliseconds(20))

        let opens = await recorder.requests().compactMap { request -> (String, UInt64?)? in
            guard case .openSession(_, let sessionID, let cursor, _) = request else { return nil }
            return (sessionID, cursor)
        }
        XCTAssertEqual(opens.count, 2)
        XCTAssertEqual(opens.last?.0, "chat-1")
        XCTAssertNil(opens.last?.1)
        let cached = await store.loadTranscript(accountID: account.id, sessionID: "chat-1")
        XCTAssertNil(cached)
    }

    func testTranscriptReplayDoesNotShowStaleErrorToast() throws {
        let model = try model()
        model.reduce(
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("error"),
                "message": .string("Old error")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertNil(model.toast)
        XCTAssertEqual(model.transcript.last?.text, "Old error")
    }
}

final class FileTreeNodeTests: XCTestCase {
    func testNestsPathsFoldersFirst() {
        let tree = FileTreeNode.tree(from: [
            WorkspaceFileRecord(path: "src/main.rs", size: 10),
            WorkspaceFileRecord(path: "src/lib/mod.rs", size: 20),
            WorkspaceFileRecord(path: "README.md", size: 30),
            WorkspaceFileRecord(path: "Cargo.toml", size: 40)
        ])

        XCTAssertEqual(tree.map(\.name), ["src", "Cargo.toml", "README.md"])
        XCTAssertNil(tree[0].size)
        XCTAssertEqual(tree[1].size, 40)
        XCTAssertNil(tree[1].children)

        let src = try? XCTUnwrap(tree[0].children)
        XCTAssertEqual(src?.map(\.name), ["lib", "main.rs"])
        XCTAssertEqual(src?[0].id, "src/lib")
        XCTAssertEqual(src?[1].id, "src/main.rs")
        XCTAssertEqual(src?[0].children?.map(\.name), ["mod.rs"])
    }
}
