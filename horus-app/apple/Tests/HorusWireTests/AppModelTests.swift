import Foundation
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

@MainActor
final class AppModelTests: XCTestCase {
    private func model(
        requestSender: (@MainActor @Sendable (GatewayRequest) async throws -> Void)? = nil
    ) throws -> AppModel {
        let suiteName = UUID().uuidString
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        return AppModel(
            client: GatewayClient(),
            store: GatewayStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: suiteName)),
                transcriptDirectory: directory
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
            approval: .on,
            systemPrompt: systemPrompt
        )
    }

    private func ready(defaultConfig: VersionedAgentConfig) -> ReadyPayload {
        ReadyPayload(
            sessions: [session(state: .idle)],
            providers: [],
            defaultConfig: defaultConfig,
            models: [],
            middlewareFeatures: [],
            maxActiveSessions: 4
        )
    }

    private func sessionReady(
        latestSequence: UInt64,
        replayEpoch: String = "epoch-1"
    ) -> SessionReadyPayload {
        SessionReadyPayload(
            replayEpoch: replayEpoch,
            latestSequence: latestSequence,
            workspace: WorkspaceInfo(id: "workspace-1", path: "/srv/horus"),
            git: nil,
            session: SessionConfigured(
                sessionId: "chat-1",
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
            contributions: [],
            toolCount: 0,
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
        state: SessionActivityState,
        outcome: SessionOutcome? = nil,
        message: String? = nil,
        turnID: String? = nil,
        createdAt: Int64 = 100,
        updatedAt: Int64 = 100
    ) -> SessionRecord {
        SessionRecord(
            sessionId: "chat-1",
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

    func testUnifiedDiffRefreshesGatewayArtifactsWithoutLocalDerivation() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.selectedSessionID = "chat-1"

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

        XCTAssertTrue(model.artifacts.isEmpty)
        let requests = await recorder.requests()
        XCTAssertTrue(requests.contains {
            guard case .listArtifacts(_, let sessionID) = $0 else { return false }
            return sessionID == "chat-1"
        })
    }

    func testContributionCatalogReferencesAndWidgetsAreGeneric() throws {
        let model = try model()
        model.contributions = [FrontendContribution(
            capability: "tasks",
            count: 3,
            commands: [],
            widgets: [
                FrontendWidget(
                    id: "count",
                    slot: "header",
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
                    slot: "message_actions",
                    text: "Fork chat",
                    tone: "neutral",
                    symbol: "fork",
                    iconOnly: true,
                    progress: nil,
                    content: nil,
                    action: .capabilityCommand(
                        capability: "sessions",
                        command: "fork",
                        arguments: "",
                        target: nil
                    )
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
                slot: "message_actions",
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
                  let submittedTarget
              ) = submission.op
        else { return XCTFail("Expected a targeted capability command") }
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(capability, "sessions")
        XCTAssertEqual(command, "fork")
        XCTAssertEqual(arguments, "")
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
                webSearch: [.off, .cached, .live]
            )],
            defaultConfig: nil,
            models: [],
            middlewareFeatures: [],
            maxActiveSessions: 4
        ))

        XCTAssertNil(model.selectedSessionID)
        XCTAssertNil(model.agentDraft)
        XCTAssertEqual(model.providerDraft?.model, "gpt-5.6-sol")

        model.saveProviderAsDefault()
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        guard case .registerProvider(_, let provider) = try XCTUnwrap(requests.first) else {
            return XCTFail("Expected first-provider registration")
        }
        XCTAssertEqual(provider, model.providerDraft)

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
            approval: .on,
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
            approval: .on,
            systemPrompt: "Active"
        )
        var edited = active
        edited.systemPrompt = "Unsaved active edit"
        var gatewayDefault = active
        gatewayDefault.systemPrompt = "New chat default"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: active)
        model.agentDraft = edited

        model.applyGatewayCatalog(ReadyPayload(
            sessions: [],
            providers: [],
            defaultConfig: VersionedAgentConfig(revision: 8, config: gatewayDefault),
            models: [],
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

    func testProviderRegistrationChainsIntoActiveChatConfiguration() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        let draft = composition(systemPrompt: "Active draft")
        model.selectedSessionID = "chat-1"
        model.agentSnapshot = VersionedAgentConfig(revision: 3, config: composition())
        model.agentDraft = draft

        model.changeProviderForCurrentChat()
        try await Task.sleep(for: .milliseconds(20))

        let registrationRequests = await recorder.requests()
        let registration = try XCTUnwrap(
            registrationRequests.lazy.compactMap { request -> String? in
                guard case .registerProvider(let requestID, _) = request else { return nil }
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

        model.saveProviderAsDefault()
        try await Task.sleep(for: .milliseconds(20))

        let registrationRequests = await recorder.requests()
        let registration = try XCTUnwrap(
            registrationRequests.lazy.compactMap { request -> String? in
                guard case .registerProvider(let requestID, _) = request else { return nil }
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
        draft.approval = .allow
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
        draft.approval = .allow
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
        laterDraft.approval = .allowNetwork
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
        store.saveTranscript(
            accountID: account.id,
            sessionID: "chat-1",
            replayEpoch: "epoch-1",
            sequence: 7,
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
        store.saveTranscript(
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
        store.saveTranscript(
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

    func testTranscriptCacheIsProtectedAndBounded() throws {
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
        for index in 0..<21 {
            store.saveTranscript(
                accountID: accountID,
                sessionID: "chat-\(index)",
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
        }
        let files = try FileManager.default.contentsOfDirectory(
            at: directory.appendingPathComponent(accountID.uuidString, isDirectory: true),
            includingPropertiesForKeys: nil
        )
        XCTAssertEqual(files.count, 20)
        #if os(iOS) && !targetEnvironment(simulator)
        let attributes = try FileManager.default.attributesOfItem(atPath: XCTUnwrap(files.first).path)
        XCTAssertEqual(attributes[.protectionKey] as? FileProtectionType, .complete)
        #endif

        let oversizedAccountID = UUID()
        store.saveTranscript(
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
        XCTAssertNil(store.loadTranscript(
            accountID: oversizedAccountID,
            sessionID: "large"
        ))

        store.saveTranscript(
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
        XCTAssertNil(store.loadTranscript(
            accountID: oversizedAccountID,
            sessionID: "corrupt"
        ))
        XCTAssertFalse(FileManager.default.fileExists(atPath: oversizedURL.path))
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
        store.saveTranscript(
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
        XCTAssertNil(store.loadTranscript(accountID: account.id, sessionID: "chat-1"))
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
