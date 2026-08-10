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

    func requestCount() -> Int {
        recorded.count
    }

    func firstRequest(
        after index: Int,
        matching predicate: @Sendable (GatewayRequest) -> Bool
    ) async -> GatewayRequest? {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(1))
        repeat {
            if let request = recorded.dropFirst(index).first(where: predicate) {
                return request
            }
            try? await Task.sleep(for: .milliseconds(5))
        } while clock.now < deadline
        return recorded.dropFirst(index).first(where: predicate)
    }
}

private actor AsyncGate {
    private var isOpen = false
    private var waiter: CheckedContinuation<Void, Never>?

    func wait() async {
        guard !isOpen else { return }
        await withCheckedContinuation { waiter = $0 }
    }

    func open() {
        isOpen = true
        waiter?.resume()
        waiter = nil
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

private extension AppModel {
    func reduce(
        event: AgentEventRecord,
        blocks: [FrontendBlock],
        history: [RenderedEventRecord]? = nil,
        preview: RenderedPreview?
    ) {
        _ = history
        let renderedBlocks: [RenderedBlock]
        if blocks.isEmpty,
           event.msg["frontendType"]?.stringValue == "render",
           let capability = event.msg["capability"]?.stringValue,
           let value = event.msg["block"],
           let block = try? FrontendBlock(json: value) {
            renderedBlocks = [RenderedBlock(capability: capability, block: block)]
        } else {
            renderedBlocks = blocks.map { RenderedBlock(capability: "test", block: $0) }
        }
        reduce(record: RecordedEvent(
            sequence: 1,
            recordedAtMs: 1_000,
            event: event,
            streamMetrics: [],
            blocks: renderedBlocks,
            preview: preview
        ))
    }
}

private extension GatewayEnvelope {
    static func agentEvent(
        sessionID: String,
        sequence: UInt64,
        event: AgentEventRecord,
        blocks: [FrontendBlock],
        history: [RenderedEventRecord]? = nil,
        preview: RenderedPreview?
    ) -> Self {
        _ = history
        return .agentEvent(
            sessionID: sessionID,
            record: RecordedEvent(
                sequence: sequence,
                recordedAtMs: 1_000,
                event: event,
                streamMetrics: [],
                blocks: blocks.map { RenderedBlock(capability: "test", block: $0) },
                preview: preview
            )
        )
    }
}

@MainActor
final class AppModelTests: XCTestCase {
    private func model(
        requestSender: (@MainActor @Sendable (GatewayRequest) async throws -> Void)? = nil,
        titleWriter: ChatTitleWriter? = nil
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
            requestSender: requestSender,
            titleWriter: titleWriter
        )
    }

    private func eventually(
        timeout: Duration = .seconds(1),
        _ predicate: @MainActor () async -> Bool
    ) async -> Bool {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: timeout)
        repeat {
            if await predicate() { return true }
            try? await Task.sleep(for: .milliseconds(5))
        } while clock.now < deadline
        return await predicate()
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
            systemPrompt: systemPrompt,
            maxModelSteps: 256
        )
    }

    private func providerStatus(
        for config: ProviderConfig,
        models: [ProviderModel] = [],
        label: String = "OpenAI"
    ) -> ProviderStatus {
        ProviderStatus(
            provider: config.provider,
            label: label,
            symbol: "chat_gpt",
            description: "Test provider",
            configured: true,
            selection: config,
            auth: .apiKey,
            defaultBaseUrl: config.baseUrl,
            defaultApiKeyEnv: "OPENAI_API_KEY",
            models: models,
            modelIds: [],
            reasoningEfforts: [],
            modelIdsConfigurable: false,
            webSearch: [config.webSearch]
        )
    }

    private func ready(
        defaultConfig: VersionedAgentConfig,
        sessions: [SessionRecord]? = nil
    ) -> ReadyPayload {
        ReadyPayload(
            machineName: "snowwhite.local",
            sessions: sessions ?? [session(state: .idle)],
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

    private func editableWidget(
        input: String = "Original input",
        capability: String = "notes",
        id: String = "queued"
    ) -> MountedWidget {
        MountedWidget(
            capability: capability,
            widget: FrontendWidget(
                id: id,
                slot: .transcriptTail,
                text: input,
                tone: "neutral",
                symbol: nil,
                iconOnly: false,
                progress: nil,
                content: nil,
                action: .capabilityCommand(
                    capability: capability,
                    command: "edit",
                    arguments: "item-1",
                    input: input,
                    target: nil
                )
            )
        )
    }

    @discardableResult
    private func beginComposerEdit(
        in model: AppModel,
        recorder: GatewayRequestRecorder,
        account: GatewayAccount,
        sessionID: String = "chat-1"
    ) async throws -> Submission {
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready
        model.selectedSessionID = sessionID
        model.composer = "Displaced draft"
        let requestCount = await recorder.requestCount()
        model.editWidgetInputInComposer(editableWidget())
        let request = await recorder.firstRequest(after: requestCount) {
            if case .submit = $0 { return true }
            return false
        }
        let submission = try XCTUnwrap(request.flatMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        })
        model.reduce(
            event: AgentEventRecord(submissionId: submission.id, msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("remove_widget"),
                "capability": .string("notes"),
                "id": .string("queued")
            ])),
            blocks: [],
            preview: nil
        )
        return submission
    }

    private func sessionReady(
        latestSequence: UInt64,
        nextBeforeSequence: UInt64? = nil,
        sessionID: String = "chat-1",
        contributions: [FrontendContribution] = [],
        widgets: [SessionWidget] = [],
        runStats: RunStats = RunStats()
    ) -> SessionReadyPayload {
        SessionReadyPayload(
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

    private func openNewSession(
        in model: AppModel,
        recorder: GatewayRequestRecorder,
        account: GatewayAccount,
        sessionID: String = "chat-1"
    ) async throws {
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready
        let requestCount = await recorder.requestCount()
        model.chooseWorkspace("/srv/horus")
        let request = await recorder.firstRequest(after: requestCount) {
            if case .createSession = $0 { return true }
            return false
        }
        guard case .createSession(let requestID, _) = try XCTUnwrap(request) else {
            return XCTFail("Expected a create-session request")
        }
        model.handle(.sessionOpened(
            requestID: requestID,
            payload: sessionReady(latestSequence: 0, sessionID: sessionID)
        ))
        model.handle(.sessionReplayComplete(requestID: requestID, sessionID: sessionID))
        guard !model.canCreateSession else { return }
        let ready = expectation(description: "New session finished loading")
        withObservationTracking {
            _ = model.canCreateSession
        } onChange: {
            ready.fulfill()
        }
        if !model.canCreateSession {
            await fulfillment(of: [ready], timeout: 1)
        }
        XCTAssertTrue(model.canCreateSession)
    }

    @discardableResult
    private func submitMessage(
        _ text: String,
        in model: AppModel,
        recorder: GatewayRequestRecorder,
        sessionID: String = "chat-1"
    ) async throws -> Submission {
        model.composer = text
        let requestCount = await recorder.requestCount()
        model.sendMessage()
        let request = await recorder.firstRequest(after: requestCount) { request in
            guard case .submit(let submittedSessionID, _) = request else { return false }
            return submittedSessionID == sessionID
        }
        let submission = try XCTUnwrap(request.flatMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        })
        try await Task.sleep(for: .milliseconds(30))
        return submission
    }

    private func renderEvent(
        capability: String = "tools",
        id: String = "result",
        group: String? = "turn",
        append: Bool = false,
        pending: Bool = false,
        text: String,
        format: String = "plain_text",
        tone: String = "neutral",
        files: [SessionFileReference] = []
    ) -> AgentEventRecord {
        AgentEventRecord(submissionId: nil, msg: .object([
            "type": .string("frontend"),
            "frontendType": .string("render"),
            "capability": .string(capability),
            "block": .object([
                "id": .string(id),
                "group": group.map(JSONValue.string) ?? .null,
                "update": .string(append ? "append" : "replace"),
                "state": .string(pending ? "pending" : "complete"),
                "role": .string("tool"),
                "title": .string("Tool"),
                "text": .string(text),
                "symbol": .null,
                "format": .string(format),
                "tone": .string(tone),
                "files": .array(files.map { file in
                    .object([
                        "id": .string(file.id),
                        "name": .string(file.name),
                        "size": .number(Double(file.size)),
                        "mediaType": .string(file.mediaType)
                    ])
                })
            ])
        ]))
    }

    private func recorded(
        _ sequence: UInt64,
        _ msg: JSONValue,
        blocks: [RenderedBlock] = []
    ) -> RecordedEvent {
        RecordedEvent(
            sequence: sequence,
            recordedAtMs: Int64(sequence * 100),
            event: AgentEventRecord(submissionId: nil, msg: msg),
            streamMetrics: [],
            blocks: blocks,
            preview: nil
        )
    }

    private func session(
        sessionID: String = "chat-1",
        state: SessionActivityState,
        outcome: SessionOutcome? = nil,
        message: String? = nil,
        turnID: String? = nil,
        executionStats: ExecutionStats = ExecutionStats(),
        createdAt: Int64 = 100,
        updatedAt: Int64 = 100,
        firstUserMessage: String? = "Review",
        title: String? = nil,
        workspaceLabel: String = "/srv/horus"
    ) -> SessionRecord {
        SessionRecord(
            sessionId: sessionID,
            sessionContext: SessionContext(
                tenantId: nil,
                userId: nil,
                userName: nil,
                workspaceId: "workspace-1",
                workspaceLabel: workspaceLabel,
                originLabel: nil
            ),
            parentSessionId: nil,
            parentSequence: nil,
            sequence: 1,
            catalogVisible: true,
            firstUserMessage: firstUserMessage,
            executionStats: executionStats,
            title: title,
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

    func testSessionSnapshotRestoresActiveTurnInterrupt() async throws {
        let recorder = GatewayRequestRecorder()
        let interruptSent = expectation(description: "Interrupt sent")
        let model = try model { request in
            await recorder.record(request)
            guard case .submit(_, let submission) = request,
                  case .interrupt = submission.op
            else { return }
            interruptSent.fulfill()
        }
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        var stats = RunStats()
        stats.active = RunSummary(
            sessionId: "chat-1",
            submissionId: "submission-1",
            turnId: "turn-1",
            startedAtMs: 1_000,
            finishedAtMs: nil,
            elapsedMs: 500,
            outcome: nil,
            modelCalls: 1,
            toolCalls: 0,
            failedToolCalls: 0,
            usage: TokenUsage()
        )

        model.handle(.sessionChanged(sessionReady(latestSequence: 8, runStats: stats)))

        XCTAssertEqual(model.activeTurnID, "turn-1")
        model.interrupt()
        await fulfillment(of: [interruptSent], timeout: 1)
        let requests = await recorder.requests()
        guard case .submit(let sessionID, let submission) = try XCTUnwrap(requests.last),
              case .interrupt(let turnID) = submission.op
        else { return XCTFail("Expected active-turn interrupt") }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(turnID, "turn-1")
    }

    func testStreamDeltasStayBatchedUntilTheCanonicalMessage() async throws {
        let model = try model()

        for _ in 0..<100 {
            model.reduce(
                event: AgentEventRecord(submissionId: nil, msg: .object([
                    "type": .string("agent_message_content_delta"),
                    "modelStepId": .string("answer-1"),
                    "phase": .string("final_answer"),
                    "delta": .string("x")
                ])),
                blocks: [],
                preview: nil
            )
        }
        XCTAssertTrue(model.transcript.isEmpty)

        let expected = String(repeating: "x", count: 100)
        let deltasFlushed = await eventually {
            model.transcript.map(\.text) == [expected]
        }
        XCTAssertTrue(deltasFlushed)
        XCTAssertEqual(model.transcript.map(\.text), [expected])
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

    func testCommentaryAndFinalAnswerRemainSeparateAssistantMessages() throws {
        let model = try model()

        for (phase, delta) in [("commentary", "Checking **the workspace**"), ("final_answer", "Done")] {
            model.reduce(
                event: AgentEventRecord(submissionId: nil, msg: .object([
                    "type": .string("agent_message_content_delta"),
                    "modelStepId": .string("response-1"),
                    "phase": .string(phase),
                    "delta": .string(delta)
                ])),
                blocks: [],
                preview: nil
            )
        }

        for (phase, message) in [("commentary", "Checking **the workspace**"), ("final_answer", "Done")] {
            model.reduce(
                event: AgentEventRecord(submissionId: nil, msg: .object([
                    "type": .string("agent_message"),
                    "phase": .string(phase),
                    "message": .string(message)
                ])),
                blocks: [],
                preview: nil
            )
        }

        XCTAssertEqual(model.transcript.map(\.text), ["Checking **the workspace**", "Done"])
        XCTAssertEqual(model.transcript.map(\.kind), [.commentary, .assistant])
        XCTAssertTrue(model.transcript.allSatisfy { !$0.pending })
    }

    func testCompletedModelStepSnapshotMatchesReplayWithoutDeltas() throws {
        let live = try model()
        let replay = try model()
        let stepID = "step-1"
        let deltas: [(String, String, String?)] = [
            ("agent_reasoning_content_delta", "Reason", nil),
            ("agent_message_content_delta", "Checking", "commentary"),
            ("agent_message_content_delta", "Done", "final_answer"),
        ]
        for (offset, delta) in deltas.enumerated() {
            var fields: [String: JSONValue] = [
                "type": .string(delta.0),
                "sessionId": .string("chat-1"),
                "turnId": .string("turn-1"),
                "modelStepId": .string(stepID),
                "delta": .string(delta.1),
            ]
            if let phase = delta.2 { fields["phase"] = .string(phase) }
            live.reduce(record: recorded(UInt64(offset + 1), .object(fields)))
        }
        let completion = recorded(4, .object([
            "type": .string("model_step_completed"),
            "sessionId": .string("chat-1"),
            "turnId": .string("turn-1"),
            "modelStepId": .string(stepID),
            "stepIndex": .number(0),
            "startedAtMs": .number(100),
            "completedAtMs": .number(400),
            "outcome": .object([
                "status": .string("completed"),
                "endTurn": .bool(true),
                "toolCallIds": .array([]),
                "usage": .object([
                    "inputTokens": .number(10),
                    "cachedInputTokens": .number(2),
                    "cacheWriteInputTokens": .number(0),
                    "outputTokens": .number(3),
                    "reasoningOutputTokens": .number(1),
                    "totalTokens": .number(13),
                ]),
                "content": .array([
                    .object([
                        "outputIndex": .number(0),
                        "partIndex": .number(0),
                        "phase": .string("reasoning"),
                        "text": .string("Reason"),
                        "annotations": .array([]),
                    ]),
                    .object([
                        "outputIndex": .number(1),
                        "partIndex": .number(0),
                        "phase": .string("commentary"),
                        "text": .string("Checking"),
                        "annotations": .array([]),
                    ]),
                    .object([
                        "outputIndex": .number(1),
                        "partIndex": .number(1),
                        "phase": .string("final_answer"),
                        "text": .string("Done"),
                        "annotations": .array([]),
                    ]),
                ]),
            ]),
        ]))

        live.reduce(record: completion)
        replay.reduce(record: completion)

        let liveProjection = live.transcript.map {
            [$0.id, $0.kind.rawValue, $0.text, String($0.pending), $0.modelStepID ?? ""]
        }
        let replayProjection = replay.transcript.map {
            [$0.id, $0.kind.rawValue, $0.text, String($0.pending), $0.modelStepID ?? ""]
        }
        XCTAssertEqual(liveProjection, replayProjection)
        XCTAssertEqual(live.transcript.map(\.text), ["Reason", "Checking", "Done"])
    }

    func testFailedModelStepKeepsItsPartialDelta() throws {
        let model = try model()
        model.reduce(record: recorded(1, .object([
            "type": .string("agent_reasoning_content_delta"),
            "sessionId": .string("chat-1"),
            "turnId": .string("turn-1"),
            "modelStepId": .string("step-1"),
            "delta": .string("Partial reasoning"),
        ])))
        model.reduce(record: recorded(2, .object([
            "type": .string("model_step_completed"),
            "sessionId": .string("chat-1"),
            "turnId": .string("turn-1"),
            "modelStepId": .string("step-1"),
            "stepIndex": .number(0),
            "startedAtMs": .number(100),
            "completedAtMs": .number(200),
            "outcome": .object(["status": .string("failed")]),
        ])))

        XCTAssertEqual(model.transcript.map(\.text), ["Partial reasoning"])
        XCTAssertFalse(try XCTUnwrap(model.transcript.first).pending)
    }

    func testActiveRunAllowsSessionNavigationButNotSelectedSessionMutation() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.activeTurnID = "turn-1"
        model.pendingApproval = PendingApproval(
            id: "approval-1",
            reason: "Approve this tool?",
            calls: []
        )
        model.gitStatus = GitStatus(currentBranch: "main", branches: ["feature", "main"])

        XCTAssertTrue(model.canOpenSession)
        XCTAssertTrue(model.canCreateSession)
        XCTAssertFalse(model.canModifySelectedSession)

        model.switchGitBranch(to: "feature")
        model.openSession("chat-2")
        try await Task.sleep(for: .milliseconds(30))

        let requests = await recorder.requests()
        XCTAssertFalse(requests.contains { request in
            if case .switchGitBranch = request { return true }
            return false
        })
        XCTAssertTrue(requests.contains { request in
            guard case .openSession(_, "chat-2", _) = request else { return false }
            return true
        })
    }

    func testActiveRunAllowsCreatingAnotherSession() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.activeTurnID = "turn-1"

        model.chooseWorkspace("/srv/another-project")
        try await Task.sleep(for: .milliseconds(20))

        let requests = await recorder.requests()
        XCTAssertTrue(requests.contains { request in
            guard case .createSession(_, "/srv/another-project") = request else { return false }
            return true
        })
    }

    func testTaskCompleteFlushesPendingReasoning() throws {
        let model = try model()

        for delta in ["think", "ing"] {
            model.reduce(
                event: AgentEventRecord(submissionId: nil, msg: .object([
                    "type": .string("agent_reasoning_content_delta"),
                    "modelStepId": .string("reasoning-1"),
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

    func testOnlyLatestActivityStepIsActiveDuringTurn() throws {
        let model = try model()
        model.activeTurnID = "turn-1"
        model.transcript = [
            TranscriptEntry(
                id: "reasoning-1",
                text: "Considering the request",
                kind: .reasoning,
                format: "plain_text",
                pending: true
            ),
            TranscriptEntry(
                id: "tools/turn-1/call-1",
                text: "Read the file",
                kind: .event,
                group: "tools/turn-1",
                format: "plain_text",
                pending: false
            ),
            TranscriptEntry(
                id: "tools/turn-1/call-2",
                text: "Run the tests",
                kind: .event,
                group: "tools/turn-1",
                format: "plain_text",
                pending: true
            ),
        ]

        XCTAssertEqual(model.activeTranscriptStepID, "tools/turn-1/call-2")

        model.transcript.append(TranscriptEntry(
            id: "answer-1",
            text: "Here is the answer",
            kind: .assistant,
            format: "plain_text",
            pending: true
        ))
        XCTAssertNil(model.activeTranscriptStepID)

        model.transcript.removeLast()
        model.activeTurnID = nil
        XCTAssertNil(model.activeTranscriptStepID)
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

    func testHorusDiagnosticsConsentDefaultsOffAndPersists() throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let model = AppModel(
            client: GatewayClient(),
            store: GatewayStore(defaults: defaults),
            settingsDefaults: defaults
        )

        XCTAssertFalse(model.sharesHorusDiagnostics)

        model.setSharesHorusDiagnostics(true)

        let relaunched = AppModel(
            client: GatewayClient(),
            store: GatewayStore(defaults: defaults),
            settingsDefaults: defaults
        )
        XCTAssertTrue(relaunched.sharesHorusDiagnostics)

        relaunched.setSharesHorusDiagnostics(false)
        XCTAssertFalse(defaults.bool(forKey: "shares-horus-diagnostics"))
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
        let requestCount = await recorder.requestCount()
        model.switchGitBranch(to: "feature")
        let request = await recorder.firstRequest(after: requestCount) { request in
            if case .switchGitBranch = request { return true }
            return false
        }

        let requests = await recorder.requests()
        XCTAssertEqual(requests.count, 1)
        guard case .switchGitBranch(_, let sessionID, let branch) = try XCTUnwrap(request) else {
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
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.activeTurnID = "turn-1"
        model.activeOperation = "steer"
        model.composer = "Use the smaller patch"

        var requestCount = await recorder.requestCount()
        model.sendMessage()
        let firstRequest = await recorder.firstRequest(after: requestCount) { request in
            if case .submit = request { return true }
            return false
        }
        let first = try XCTUnwrap(firstRequest.flatMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        })
        model.reduce(
            event: AgentEventRecord(submissionId: first.id, msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("widget"),
                "capability": .string("steering"),
                "item": .object([
                    "id": .string("queued"),
                    "slot": .string("transcript_tail"),
                    "text": .string("Use the smaller patch"),
                    "tone": .string("neutral"),
                    "symbol": .null,
                    "iconOnly": .bool(false),
                    "progress": .null,
                    "content": .null,
                    "action": .object([
                        "type": .string("capability_command"),
                        "capability": .string("steering"),
                        "command": .string("edit"),
                        "arguments": .string(first.id),
                        "input": .string("Use the smaller patch"),
                        "target": .null
                    ])
                ])
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
        XCTAssertEqual(model.transcriptTailWidgets.first?.widget.text, "Use the smaller patch")
        XCTAssertEqual(
            model.transcriptTailWidgets.first?.widget.action?.capabilityInput,
            "Use the smaller patch"
        )

        model.connectionState = .ready
        model.composer = "Retry this steering"
        requestCount = await recorder.requestCount()
        model.sendMessage()
        let secondRequest = await recorder.firstRequest(after: requestCount) { request in
            if case .submit = request { return true }
            return false
        }
        let second = try XCTUnwrap(secondRequest.flatMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        })
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

    func testSteeringFeedbackWaitsUntilTheQueuedMessageReachesTheAgent() throws {
        let model = try model()
        model.contributions = [FrontendContribution(
            capability: "steering",
            acceptsFileAttachments: false,
            count: nil,
            commands: [],
            widgets: [],
            references: [],
            activeInput: FrontendActiveInput(operation: "steer")
        )]
        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("task_started"),
                "turnId": .string("turn-1")
            ])),
            blocks: [],
            preview: nil
        )
        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("user_message"),
                "message": .string("Start"),
                "attachments": .array([]),
                "messageTarget": .object([
                    "checkpointSequence": .number(1),
                    "batchItemCount": .number(1)
                ])
            ])),
            blocks: [],
            preview: nil
        )
        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("remove_widget"),
                "capability": .string("steering"),
                "id": .string("queued")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.steeringDeliveryRevision, 0)

        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("user_message"),
                "message": .string("Use the smaller patch"),
                "attachments": .array([]),
                "messageTarget": .object([
                    "checkpointSequence": .number(2),
                    "batchItemCount": .number(1)
                ])
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.steeringDeliveryRevision, 1)
    }

    func testQueuedWidgetEditIsTakenBeforeTheComposerResubmitsFreshActiveInput() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        let target = MessageTarget(checkpointSequence: 12, batchItemCount: 3)
        let queued = MountedWidget(
            capability: "notes",
            widget: FrontendWidget(
                id: "queued",
                slot: .transcriptTail,
                text: "Queued note",
                tone: "neutral",
                symbol: nil,
                iconOnly: false,
                progress: nil,
                content: nil,
                action: .capabilityCommand(
                    capability: "notes",
                    command: "edit",
                    arguments: "note-1",
                    input: "Original input",
                    target: target
                )
            )
        )
        let sibling = MountedWidget(
            capability: "notes",
            widget: FrontendWidget(
                id: "sibling",
                slot: .transcriptTail,
                text: "Another queued note",
                tone: "neutral",
                symbol: nil,
                iconOnly: false,
                progress: nil,
                content: nil,
                action: nil
            )
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.activeTurnID = "turn-1"
        model.activeOperation = "steer"
        model.mountedWidgets = [queued, sibling]
        model.composer = "Keep this draft"
        let focusRequest = model.composerFocusRequest

        var requestCount = await recorder.requestCount()
        model.editWidgetInputInComposer(queued)
        let editRequest = await recorder.firstRequest(after: requestCount) { request in
            if case .submit = request { return true }
            return false
        }
        guard case .submit(let sessionID, let editSubmission) = try XCTUnwrap(editRequest),
              case .capabilityCommand(
                  let capability,
                  let command,
                  let arguments,
                  let input,
                  let submittedTarget
              ) = editSubmission.op
        else { return XCTFail("Expected the queued capability operation") }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(capability, "notes")
        XCTAssertEqual(command, "edit")
        XCTAssertEqual(arguments, "note-1")
        XCTAssertEqual(input, "Original input")
        XCTAssertEqual(submittedTarget, target)

        model.handle(.accepted(requestID: editSubmission.id))
        XCTAssertEqual(model.composer, "Keep this draft")
        XCTAssertFalse(model.canSendComposer)
        XCTAssertEqual(model.composerFocusRequest, focusRequest)
        XCTAssertEqual(model.transcriptTailWidgets.map(\.id), [queued.id, sibling.id])

        model.reduce(
            event: AgentEventRecord(submissionId: editSubmission.id, msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("remove_widget"),
                "capability": .string("notes"),
                "id": .string("queued")
            ])),
            blocks: [],
            preview: nil
        )
        XCTAssertEqual(model.composer, "Original input")
        XCTAssertTrue(model.canSendComposer)
        XCTAssertEqual(model.composerFocusRequest, focusRequest + 1)
        XCTAssertEqual(model.transcriptTailWidgets.map(\.id), [sibling.id])

        model.composer = "Edited input"
        requestCount = await recorder.requestCount()
        model.sendMessage()
        let editedRequest = await recorder.firstRequest(after: requestCount) { request in
            if case .submit = request { return true }
            return false
        }
        let editedSubmission = try XCTUnwrap(editedRequest.flatMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        })
        guard case .activeInput(let operation, let turnID, let text) = editedSubmission.op
        else { return XCTFail("Expected fresh active input") }
        XCTAssertEqual(operation, "steer")
        XCTAssertEqual(turnID, "turn-1")
        XCTAssertEqual(text, "Edited input")
        XCTAssertEqual(model.composer, "Keep this draft")

        model.reduce(
            event: AgentEventRecord(submissionId: editedSubmission.id, msg: .object([
                "type": .string("task_started"),
                "turnId": .string("turn-1")
            ])),
            blocks: [],
            preview: nil
        )
        XCTAssertFalse(model.canSendComposer)
        model.handle(.rejected(GatewayRejection(
            requestId: editedSubmission.id,
            code: "queue_full",
            message: "Try again",
            fatal: false
        )))
        XCTAssertEqual(model.composer, "Edited input")
        XCTAssertTrue(model.canSendComposer)
    }

    func testComposerEditRecoveryRestoresEditedTextAndDisplacedDraftAfterRelaunch() async throws {
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
            transcriptDirectory: root.appendingPathComponent("Transcripts"),
            draftDirectory: root.appendingPathComponent("Drafts")
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        await store.saveComposerDraft(
            "Displaced draft",
            accountID: account.id,
            sessionID: "chat-1"
        )
        try await store.saveComposerEditRecovery(
            ComposerEditRecovery(
                capability: "notes",
                widgetID: "queued",
                originalInput: "Original input",
                displacedDraft: "Displaced draft",
                editedInput: "Edited after relaunch",
                requestID: "removed-input",
                submissionBaselineSequence: nil,
                phase: .editing
            ),
            accountID: account.id,
            sessionID: "chat-1"
        )
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
        let openRequests = await recorder.requests()
        let openRequest = try XCTUnwrap(openRequests.last)
        guard case .openSession(let openID, _, _) = openRequest else {
            return XCTFail("Expected session open")
        }
        model.handle(.sessionOpened(
            requestID: openID,
            payload: sessionReady(latestSequence: 0, sessionID: "chat-1")
        ))
        model.handle(.sessionReplayComplete(requestID: openID, sessionID: "chat-1"))
        try await Task.sleep(for: .milliseconds(100))

        XCTAssertEqual(model.composer, "Edited after relaunch")
        XCTAssertTrue(model.canSendComposer)

        model.sendMessage()
        try await Task.sleep(for: .milliseconds(50))
        let submissions = await recorder.requests().compactMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        }
        guard case .userInput(let text, _) = try XCTUnwrap(submissions.last).op else {
            return XCTFail("Expected recovered user input")
        }
        XCTAssertEqual(text, "Edited after relaunch")
        XCTAssertEqual(model.composer, "Displaced draft")
    }

    func testComposerEditRecoveryRecognizesSubmissionReplayedBeforeItsDiskLoad() async throws {
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
            transcriptDirectory: root.appendingPathComponent("Transcripts"),
            draftDirectory: root.appendingPathComponent("Drafts")
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        await store.saveComposerDraft(
            "Displaced draft",
            accountID: account.id,
            sessionID: "chat-1"
        )
        try await store.saveComposerEditRecovery(
            ComposerEditRecovery(
                capability: "notes",
                widgetID: "queued",
                originalInput: "Original input",
                displacedDraft: "Displaced draft",
                editedInput: "Edited input",
                requestID: "submitted-edit",
                submissionBaselineSequence: 10,
                phase: .submitting
            ),
            accountID: account.id,
            sessionID: "chat-1"
        )
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
        let openRequestCount = await recorder.requestCount()
        model.openSession("chat-1")
        let openRequest = await recorder.firstRequest(after: openRequestCount) { request in
            guard case .openSession(_, "chat-1", _) = request else { return false }
            return true
        }
        guard case .openSession(let openID, _, _) = try XCTUnwrap(openRequest) else {
            return XCTFail("Expected session open")
        }

        model.handle(.sessionOpened(
            requestID: openID,
            payload: sessionReady(latestSequence: 11, sessionID: "chat-1")
        ))
        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 11,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("user_message"),
                "message": .string("Edited input"),
                "attachments": .array([]),
                "messageTarget": .object([
                    "checkpointSequence": .number(11),
                    "batchItemCount": .number(1)
                ])
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))
        model.handle(.sessionReplayComplete(requestID: openID, sessionID: "chat-1"))
        try await Task.sleep(for: .milliseconds(150))

        XCTAssertEqual(model.composer, "Displaced draft")
        XCTAssertEqual(model.transcript.filter { $0.kind == .user }.map(\.text), ["Edited input"])
        XCTAssertTrue(model.canSendComposer)
        let replayedRecovery = await store.loadComposerEditRecovery(
            accountID: account.id,
            sessionID: "chat-1"
        )
        XCTAssertNil(replayedRecovery)
    }

    func testCompletedComposerEditTombstoneIsIgnoredAndOverwrittenByTheNextEdit() async throws {
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
            transcriptDirectory: root.appendingPathComponent("Transcripts"),
            draftDirectory: root.appendingPathComponent("Drafts")
        )
        let accountID = UUID()
        let completed = ComposerEditRecovery(
            capability: "notes",
            widgetID: "queued",
            originalInput: "Original",
            displacedDraft: "Draft",
            editedInput: "Edited",
            requestID: "submitted",
            submissionBaselineSequence: 7,
            phase: .completed
        )
        try await store.saveComposerEditRecovery(
            completed,
            accountID: accountID,
            sessionID: "chat-1"
        )
        let ignored = await store.loadComposerEditRecovery(
            accountID: accountID,
            sessionID: "chat-1"
        )
        XCTAssertNil(ignored)

        var next = completed
        next.requestID = "next-edit"
        next.submissionBaselineSequence = nil
        next.phase = .editing
        try await store.saveComposerEditRecovery(
            next,
            accountID: accountID,
            sessionID: "chat-1"
        )
        let restored = await store.loadComposerEditRecovery(
            accountID: accountID,
            sessionID: "chat-1"
        )
        XCTAssertEqual(restored, next)
    }

    func testForgettingGatewayInvalidatesItsInMemoryComposerEdit() async throws {
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
            transcriptDirectory: root.appendingPathComponent("Transcripts"),
            draftDirectory: root.appendingPathComponent("Drafts")
        )
        let recorder = GatewayRequestRecorder()
        let ordinaryMessageSent = expectation(description: "Ordinary message sent")
        let model = AppModel(
            client: GatewayClient(),
            store: store,
            settingsDefaults: defaults,
            requestSender: { request in
                await recorder.record(request)
                if case .submit(_, let submission) = request,
                   case .userInput(let text, _) = submission.op,
                   text == "New gateway message" {
                    ordinaryMessageSent.fulfill()
                }
            }
        )
        let first = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        let second = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9192"))
        try await beginComposerEdit(in: model, recorder: recorder, account: first)
        model.accounts = [first, second]

        let gatewayForgotten = expectation(description: "Gateway forgotten")
        withObservationTracking {
            _ = model.accounts
        } onChange: {
            gatewayForgotten.fulfill()
        }
        model.forgetSelectedGateway()
        await fulfillment(of: [gatewayForgotten], timeout: 1)
        model.selectedAccountID = second.id
        model.selectedSessionID = "chat-1"
        model.connectionState = .ready
        model.composer = "New gateway message"
        model.sendMessage()
        await fulfillment(of: [ordinaryMessageSent], timeout: 1)

        let submissions = await recorder.requests().compactMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        }
        guard case .userInput(let text, _) = try XCTUnwrap(submissions.last).op else {
            return XCTFail("Expected an ordinary new-gateway message")
        }
        XCTAssertEqual(text, "New gateway message")
    }

    func testDeletingSelectedSessionInvalidatesItsInMemoryComposerEdit() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in await recorder.record(request) })
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await beginComposerEdit(in: model, recorder: recorder, account: account)
        let selected = session(sessionID: "chat-1", state: .idle)
        model.sessions = [selected]

        model.deleteSession(selected)
        try await Task.sleep(for: .milliseconds(30))
        let requests = await recorder.requests()
        guard case .deleteSession(let deleteID, "chat-1") = try XCTUnwrap(
            requests.last(where: { if case .deleteSession = $0 { true } else { false } })
        ) else { return XCTFail("Expected session deletion") }
        model.handle(.accepted(requestID: deleteID))
        model.handle(.sessions(requestID: deleteID, sessions: []))

        model.selectedSessionID = "chat-1"
        model.connectionState = .ready
        model.composer = "Replacement message"
        model.sendMessage()
        try await Task.sleep(for: .milliseconds(30))

        let submissions = await recorder.requests().compactMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        }
        guard case .userInput(let text, _) = try XCTUnwrap(submissions.last).op else {
            return XCTFail("Expected an ordinary message after deletion")
        }
        XCTAssertEqual(text, "Replacement message")
    }

    func testSwitchingGatewayImmediatelyPersistsTheLatestComposerEdit() async throws {
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
            transcriptDirectory: root.appendingPathComponent("Transcripts"),
            draftDirectory: root.appendingPathComponent("Drafts")
        )
        let recorder = GatewayRequestRecorder()
        let model = AppModel(
            client: GatewayClient(),
            store: store,
            settingsDefaults: defaults,
            requestSender: { request in await recorder.record(request) }
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        let second = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9192"))
        model.accounts = [account, second]
        model.selectedAccountID = account.id
        model.connectionState = .ready
        let openRequestCount = await recorder.requestCount()
        model.openSession("chat-1")
        let openRequest = await recorder.firstRequest(after: openRequestCount) { request in
            guard case .openSession(_, "chat-1", _) = request else { return false }
            return true
        }
        guard case .openSession(let openID, _, _) = try XCTUnwrap(openRequest) else {
            return XCTFail("Expected session open")
        }
        model.handle(.sessionOpened(
            requestID: openID,
            payload: sessionReady(latestSequence: 0, sessionID: "chat-1")
        ))
        model.handle(.sessionReplayComplete(requestID: openID, sessionID: "chat-1"))
        let sessionReady = await eventually { model.canCreateSession }
        XCTAssertTrue(sessionReady)
        try await beginComposerEdit(in: model, recorder: recorder, account: account)
        model.accounts = [account, second]

        model.composer = "Latest edit before switching"
        XCTAssertEqual(model.composer, "Latest edit before switching")
        XCTAssertTrue(model.canSendComposer)
        model.selectAccount(second.id)
        XCTAssertEqual(model.composer, "")

        let recoverySaved = await eventually {
            await store.loadComposerEditRecovery(
                accountID: account.id,
                sessionID: "chat-1"
            )?.editedInput == "Latest edit before switching"
        }
        XCTAssertTrue(recoverySaved)
        let recovery = await store.loadComposerEditRecovery(
            accountID: account.id,
            sessionID: "chat-1"
        )
        XCTAssertEqual(recovery?.editedInput, "Latest edit before switching")
    }

    func testSendMessageCannotBypassConnectionOrPendingWidgetEdit() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.selectedSessionID = "chat-1"
        model.composer = "Do not lose this"
        model.contributions = [fileAttachmentContribution()]

        model.sendMessage()
        try await Task.sleep(for: .milliseconds(20))
        let disconnectedRequests = await recorder.requests()
        XCTAssertTrue(disconnectedRequests.isEmpty)
        XCTAssertEqual(model.composer, "Do not lose this")

        model.connectionState = .ready
        XCTAssertTrue(model.canImportAttachments)
        model.editWidgetInputInComposer(editableWidget())
        XCTAssertFalse(model.canImportAttachments)
        let requestCount = await recorder.requestCount()
        model.sendMessage()
        let request = await recorder.firstRequest(after: requestCount) { request in
            guard case .submit(_, let submission) = request,
                  case .capabilityCommand = submission.op
            else { return false }
            return true
        }

        let requests = await recorder.requests()
        XCTAssertEqual(requests.count, requestCount + 1)
        guard case .submit(_, let submission) = try XCTUnwrap(request),
              case .capabilityCommand = submission.op
        else { return XCTFail("Expected only the edit-removal command") }
        XCTAssertEqual(model.composer, "Do not lose this")
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
        let readyRequests = await recorder.requests()
        let openRequest = try XCTUnwrap(readyRequests.last(where: {
            if case .openSession = $0 { return true }
            return false
        }))
        guard case .openSession(let openRequestID, _, _) = openRequest else {
            return XCTFail("Expected session open")
        }
        await harness.yield(.sessionOpened(
            requestID: openRequestID,
            payload: sessionReady(latestSequence: 0)
        ))
        await harness.yield(.sessionReplayComplete(
            requestID: openRequestID,
            sessionID: "chat-1"
        ))
        try await Task.sleep(for: .milliseconds(50))
        model.composer = "Run this once"
        XCTAssertTrue(model.canSendComposer)
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

    func testCanonicalFrontendRenderIsCapabilityScopedAndAppliedOnce() throws {
        let app = try model()
        let first = renderEvent(
            pending: true,
            text: "Started",
            tone: "warning"
        )
        let started = RenderedBlock(capability: "tools", block: FrontendBlock(
            id: "result",
            group: "turn",
            update: .replace,
            state: .pending,
            role: .tool,
            title: "Tool",
            text: "Started",
            symbol: nil,
            format: "plain_text",
            tone: "warning",
            files: []
        ))
        let finishedEvent = renderEvent(
            group: nil,
            append: true,
            text: " and finished",
            tone: "success"
        )
        let finished = RenderedBlock(capability: "tools", block: FrontendBlock(
            id: "result",
            group: nil,
            update: .append,
            state: .complete,
            role: .tool,
            title: "Tool",
            text: " and finished",
            symbol: nil,
            format: "plain_text",
            tone: "success",
            files: []
        ))
        let firstRecord = RecordedEvent(
            sequence: 1,
            recordedAtMs: 1_000,
            event: first,
            streamMetrics: [],
            blocks: [started],
            preview: nil
        )
        app.reduce(record: firstRecord)
        app.reduce(record: RecordedEvent(
            sequence: 2,
            recordedAtMs: 1_001,
            event: finishedEvent,
            streamMetrics: [],
            blocks: [finished],
            preview: nil
        ))

        let entry = try XCTUnwrap(app.transcript.first)
        XCTAssertEqual(app.transcript.count, 1)
        XCTAssertEqual(entry.id, "block:5:toolsresult")
        XCTAssertEqual(entry.group, "turn")
        XCTAssertEqual(entry.text, "Started and finished")
        XCTAssertEqual(entry.tone, "success")
        XCTAssertFalse(entry.pending)

        let replay = try model()
        replay.reduce(record: firstRecord)
        XCTAssertEqual(replay.transcript.first?.id, "block:5:toolsresult")
        XCTAssertEqual(replay.transcript.first?.tone, "warning")
    }

    func testRenderedBlocksPreserveCapabilityAndGroup() throws {
        let app = try model()
        for (sequence, capability) in [(UInt64(1), "tools"), (UInt64(2), "review")] {
            app.reduce(record: RecordedEvent(
                sequence: sequence,
                recordedAtMs: Int64(sequence),
                event: renderEvent(group: "turn", text: capability),
                streamMetrics: [],
                blocks: [RenderedBlock(capability: capability, block: FrontendBlock(
                    id: "result",
                    group: "turn",
                    update: .replace,
                    state: .complete,
                    role: .tool,
                    title: capability,
                    text: capability,
                    symbol: nil,
                    format: "plain_text",
                    tone: "neutral",
                    files: []
                ))],
                preview: nil
            ))
        }

        XCTAssertEqual(app.transcript.map(\.group), ["turn", "turn"])
        XCTAssertEqual(app.transcript.compactMap(\.capability), ["tools", "review"])
    }

    func testFrontendRenderCarriesFilesThroughReplacementAndAppend() throws {
        let model = try model()
        let file = SessionFileReference(
            id: "file-1",
            name: "report.xlsx",
            size: 4,
            mediaType: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        )

        model.reduce(
            event: renderEvent(pending: true, text: "Creating report"),
            blocks: [],
            preview: nil
        )
        model.reduce(
            event: renderEvent(text: "Report ready", files: [file]),
            blocks: [],
            preview: nil
        )

        let completed = try XCTUnwrap(model.transcript.first)
        XCTAssertEqual(completed.text, "Report ready")
        XCTAssertEqual(completed.files, [file])
        XCTAssertFalse(completed.pending)

        model.reduce(
            event: renderEvent(group: nil, append: true, text: "\nOpen it below."),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.transcript.first?.text, "Report ready\nOpen it below.")
        XCTAssertEqual(model.transcript.first?.files, [file])
    }

    func testPreviewPreservesRenderedBlocksAndCapabilityRender() throws {
        let model = try model()
        let outer = FrontendBlock(
            id: "tools/call",
            group: "tools/turn",
            update: .replace,
            state: .complete,
            role: .tool,
            title: "Read file",
            text: "Read file",
            symbol: "task",
            format: "plain_text",
            tone: "neutral",
            files: []
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
            RenderedEventRecord(
                event: .object(["type": .string("tool_call_end")]),
                blocks: [RenderedBlock(capability: "tools", block: outer)]
            ),
            RenderedEventRecord(
                event: rendered.msg,
                blocks: [RenderedBlock(capability: "reviewer", block: FrontendBlock(
                    id: "change",
                    group: "work",
                    update: .replace,
                    state: .complete,
                    role: .artifact,
                    title: "Code change",
                    text: "@@ -1 +1 @@",
                    symbol: nil,
                    format: "unified_diff",
                    tone: "success",
                    files: []
                ))]
            )
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
        XCTAssertEqual(snapshot.blocks.last?.block.id, "change")
        XCTAssertEqual(snapshot.blocks.last?.block.group, "work")
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
        let requestCount = await recorder.requestCount()
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
        let request = await recorder.firstRequest(after: requestCount) { request in
            guard case .submit("chat-1", _) = request else { return false }
            return true
        }
        guard case .submit(_, let submission) = try XCTUnwrap(request) else {
            return XCTFail("Expected picker submission")
        }
        let block = FrontendBlock(
            id: "worker/message",
            group: nil,
            update: .replace,
            state: .complete,
            role: .notice,
            title: "Done",
            text: "Done",
            symbol: nil,
            format: "plain_text",
            tone: "success",
            files: []
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
                RenderedEventRecord(
                    event: .object(["type": .string("agent_message")]),
                    blocks: [RenderedBlock(capability: "worker", block: block)]
                )
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
        let operationSent = expectation(description: "Frontend operation sent")
        let model = try model(requestSender: { request in
            await recorder.record(request)
            if case .submit = request { operationSent.fulfill() }
        })
        model.selectedSessionID = "chat-1"

        model.submitFrontendOperation(.capabilityCommand(
            capability: "notes",
            command: "edit",
            arguments: "note-1",
            input: "Use one row.",
            target: nil
        ))
        await fulfillment(of: [operationSent], timeout: 1)

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

        let requestCount = await recorder.requestCount()
        model.reduce(
            event: renderEvent(
                capability: "reviewer",
                text: "@@ -1 +1 @@",
                format: "unified_diff"
            ),
            blocks: [],
            preview: nil
        )
        let refresh = await recorder.firstRequest(after: requestCount) { request in
            guard case .getGitDiff(_, "chat-1", .unstaged) = request else { return false }
            return true
        }
        XCTAssertNotNil(refresh)
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

    func testSessionFileUploadUsesAcknowledgedChunksAndSendsNativeReferences() async throws {
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

        var requestCount = await recorder.requestCount()
        await model.importAttachments([fileURL])
        let begin = await recorder.firstRequest(after: requestCount) { request in
            if case .beginSessionFileUpload = request { return true }
            return false
        }
        guard case .beginSessionFileUpload(let beginID, let sessionID, let name, let size, _) = try XCTUnwrap(
            begin
        )
        else { return XCTFail("Expected session file upload start") }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(name, "scan.png")
        XCTAssertEqual(size, 3)

        requestCount = await recorder.requestCount()
        model.handle(.sessionFileUploadReady(
            requestID: beginID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            maxChunkBytes: 2
        ))
        let firstChunkRequest = await recorder.firstRequest(
            after: requestCount
        ) { request in
            if case .uploadSessionFileChunk = request { return true }
            return false
        }
        let firstChunk = try XCTUnwrap(firstChunkRequest)
        guard case .uploadSessionFileChunk(let firstID, _, _, let firstOffset, let firstData) = firstChunk else {
            return XCTFail("Expected first session file chunk")
        }
        XCTAssertEqual(firstOffset, 0)
        XCTAssertEqual(firstData, Data([1, 2]))

        requestCount = await recorder.requestCount()
        model.handle(.sessionFileUploadChunkAccepted(
            requestID: firstID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            nextOffset: 2
        ))
        let secondChunkRequest = await recorder.firstRequest(
            after: requestCount
        ) { request in
            if case .uploadSessionFileChunk = request { return true }
            return false
        }
        let secondChunk = try XCTUnwrap(secondChunkRequest)
        guard case .uploadSessionFileChunk(let secondID, _, _, let secondOffset, let secondData) = secondChunk else {
            return XCTFail("Expected second session file chunk")
        }
        XCTAssertEqual(secondOffset, 2)
        XCTAssertEqual(secondData, Data([3]))

        requestCount = await recorder.requestCount()
        model.handle(.sessionFileUploadChunkAccepted(
            requestID: secondID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            nextOffset: 3
        ))
        let finishRequest = await recorder.firstRequest(
            after: requestCount
        ) { request in
            if case .finishSessionFileUpload = request { return true }
            return false
        }
        let finish = try XCTUnwrap(finishRequest)
        guard case .finishSessionFileUpload(let finishID, _, _) = finish else {
            return XCTFail("Expected session file upload finish")
        }
        let attachment = SessionFileReference(
            id: "file-1",
            name: "scan.png",
            size: 3,
            mediaType: "image/png"
        )
        model.handle(.sessionFileUploadCompleted(
            requestID: finishID,
            sessionID: "chat-1",
            file: attachment
        ))
        XCTAssertEqual(model.sessionUploads, [attachment])
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

        requestCount = await recorder.requestCount()
        model.sendMessage()
        let submitRequest = await recorder.firstRequest(
            after: requestCount
        ) { request in
            if case .submit = request { return true }
            return false
        }
        let submit = try XCTUnwrap(submitRequest)
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
        XCTAssertEqual(model.transcript.last?.files, [attachment])
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
        let attachment = SessionFileReference(
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
        let requestCount = await recorder.requestCount()
        model.sendMessage()
        let request = await recorder.firstRequest(after: requestCount) {
            guard case .submit("chat-1", _) = $0 else { return false }
            return true
        }
        guard case .submit(_, let submission) = try XCTUnwrap(request) else {
            return XCTFail("Expected attachment submission")
        }
        guard case .userInput(_, let attachments) = submission.op else {
            return XCTFail("Expected attachment submission")
        }
        XCTAssertEqual(attachments, [attachment])
    }

    func testSessionFileUploadRejectsKnownResponseWithWrongPhase() async throws {
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

        let requestCount = await recorder.requestCount()
        await model.importAttachments([fileURL])
        let beginRequest = await recorder.firstRequest(after: requestCount) {
            if case .beginSessionFileUpload = $0 { return true }
            return false
        }
        guard case .beginSessionFileUpload(let beginID, _, _, _, _) = try XCTUnwrap(beginRequest)
        else { return XCTFail("Expected session file upload start") }

        model.handle(.sessionFileUploadChunkAccepted(
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

    func testSessionFileUploadRejectsUnexpectedAcknowledgedOffset() async throws {
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

        var requestCount = await recorder.requestCount()
        await model.importAttachments([fileURL])
        let beginRequest = await recorder.firstRequest(after: requestCount) {
            if case .beginSessionFileUpload = $0 { return true }
            return false
        }
        guard case .beginSessionFileUpload(let beginID, _, _, _, _) = try XCTUnwrap(beginRequest)
        else { return XCTFail("Expected session file upload start") }
        requestCount = await recorder.requestCount()
        model.handle(.sessionFileUploadReady(
            requestID: beginID,
            sessionID: "chat-1",
            uploadID: "upload-1",
            maxChunkBytes: 2
        ))
        let chunkRequest = await recorder.firstRequest(after: requestCount) {
            if case .uploadSessionFileChunk = $0 { return true }
            return false
        }
        guard case .uploadSessionFileChunk(let chunkID, _, _, _, _) = try XCTUnwrap(chunkRequest)
        else { return XCTFail("Expected session file chunk") }

        model.handle(.sessionFileUploadChunkAccepted(
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
            if case .uploadSessionFileChunk = $0 { return true }
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
            let attachment = SessionFileReference(
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

    func testFilesInspectorRequestsTheSelectedCollection() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"

        var requestCount = await recorder.requestCount()
        model.showFiles(.unstaged)
        let unstagedRequest = await recorder.firstRequest(after: requestCount) {
            if case .getGitDiff = $0 { return true }
            return false
        }
        let unstagedRequests = await recorder.requests()
        guard case .getGitDiff(_, let sessionID, let diffScope) = try XCTUnwrap(unstagedRequest)
        else { return XCTFail("Expected unstaged Git diff") }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(diffScope, .unstaged)
        XCTAssertFalse(unstagedRequests.contains {
            if case .listWorkspaceFiles = $0 { return true }
            return false
        })

        requestCount = await recorder.requestCount()
        model.selectFilesInspectorTab(.allFiles)
        let allRequest = await recorder.firstRequest(after: requestCount) {
            if case .listWorkspaceFiles = $0 { return true }
            return false
        }
        guard case .listWorkspaceFiles(_, _, let allScope) = try XCTUnwrap(allRequest)
        else { return XCTFail("Expected all workspace files") }
        XCTAssertEqual(allScope, .all)

        requestCount = await recorder.requestCount()
        model.selectFilesInspectorTab(.chatFiles)
        let artifactRequest = await recorder.firstRequest(after: requestCount) {
            if case .listArtifacts(_, "chat-1") = $0 { return true }
            return false
        }
        let uploadRequest = await recorder.firstRequest(after: requestCount) {
            if case .listSessionUploads(_, "chat-1") = $0 { return true }
            return false
        }
        XCTAssertNotNil(artifactRequest)
        XCTAssertNotNil(uploadRequest)
    }

    func testArtifactListIgnoresAResponseForAnotherSession() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        let file = SessionFileReference(
            id: "file-1",
            name: "diagram.svg",
            size: 3,
            mediaType: "image/svg+xml"
        )
        let artifact = ArtifactRecord(
            id: "artifact-1",
            sessionId: "chat-1",
            kind: .file,
            title: "Architecture diagram",
            block: FrontendBlock(
                id: "artifacts/file-1",
                group: nil,
                update: .replace,
                state: .complete,
                role: .artifact,
                title: "Architecture diagram",
                text: "",
                symbol: "storage",
                format: "plain_text",
                tone: "neutral",
                files: [file]
            )
        )

        model.showFiles(.chatFiles)
        try await Task.sleep(for: .milliseconds(20))
        let requests = await recorder.requests()
        guard let request = requests.last(where: {
            if case .listArtifacts = $0 { return true }
            return false
        }), case .listArtifacts(let requestID, _) = request
        else { return XCTFail("Expected artifact list request") }

        model.handle(.artifacts(
            requestID: requestID,
            sessionID: "chat-2",
            artifacts: [artifact],
            truncated: true
        ))
        XCTAssertTrue(model.artifacts.isEmpty)
        XCTAssertFalse(model.artifactsTruncated)
        XCTAssertTrue(model.isLoadingArtifacts)

        model.handle(.artifacts(
            requestID: requestID,
            sessionID: "chat-1",
            artifacts: [artifact],
            truncated: true
        ))
        XCTAssertEqual(model.artifacts, [artifact])
        XCTAssertTrue(model.artifactsTruncated)
        XCTAssertFalse(model.isLoadingArtifacts)
    }

    func testWorkspaceReferencesUseCLIFuzzyRankingAndReplacement() throws {
        let model = try model()
        model.workspaceFiles = [
            WorkspaceFileRecord(path: "examples/application.txt", size: 1),
            WorkspaceFileRecord(path: "Sources/App.swift", size: 1),
            WorkspaceFileRecord(path: "docs/My App.md", size: 1),
            WorkspaceFileRecord(path: "src/main.rs", size: 1)
        ]

        let prefix = "Review @app"
        let prefixSuggestions = try XCTUnwrap(model.referenceSuggestions(
            in: prefix,
            cursor: prefix.endIndex
        ))
        XCTAssertEqual(prefixSuggestions.matches.first?.label, "@Sources/App.swift")
        XCTAssertEqual(prefixSuggestions.matches.first?.replacement, "Sources/App.swift")

        let spaced = "Review @my"
        let spacedSuggestions = try XCTUnwrap(model.referenceSuggestions(
            in: spaced,
            cursor: spaced.endIndex
        ))
        XCTAssertEqual(spacedSuggestions.matches.first?.label, "@docs/My App.md")
        XCTAssertEqual(spacedSuggestions.matches.first?.replacement, "\"docs/My App.md\"")

        let fuzzy = "Review @smr"
        let fuzzySuggestions = try XCTUnwrap(model.referenceSuggestions(
            in: fuzzy,
            cursor: fuzzy.endIndex
        ))
        XCTAssertEqual(fuzzySuggestions.matches.first?.label, "@src/main.rs")
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

        let requestCount = await recorder.requestCount()
        model.previewWorkspaceFile(file)
        let readRequest = await recorder.firstRequest(after: requestCount) { request in
            if case .readWorkspaceFile = request { return true }
            return false
        }
        guard case .readWorkspaceFile(let readID, _, let path, let offset, _) = try XCTUnwrap(
            readRequest
        )
        else { return XCTFail("Expected workspace file read request") }
        XCTAssertEqual(path, file.path)
        XCTAssertEqual(offset, 0)
        let previewFinished = expectation(description: "Source preview finished")
        withObservationTracking {
            _ = model.textFilePreview
        } onChange: {
            previewFinished.fulfill()
        }
        model.handle(.workspaceFileChunk(
            requestID: readID,
            sessionID: "chat-1",
            path: file.path,
            offset: 0,
            data: data,
            nextOffset: nil
        ))
        await fulfillment(of: [previewFinished], timeout: 1)

        XCTAssertEqual(model.textFilePreview?.contents, contents)
        XCTAssertNil(model.previewURL)
        model.discardFilePresentation()
    }

    func testWorkspaceBinaryFileUsesQuickLookPreview() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        let file = WorkspaceFileRecord(path: "image.bin", size: 3)

        let requestCount = await recorder.requestCount()
        model.previewWorkspaceFile(file)
        let request = await recorder.firstRequest(after: requestCount) { request in
            if case .readWorkspaceFile = request { return true }
            return false
        }
        guard case .readWorkspaceFile(let requestID, _, _, _, _) = try XCTUnwrap(request)
        else { return XCTFail("Expected workspace file read request") }
        let previewFinished = expectation(description: "Binary preview finished")
        withObservationTracking {
            _ = model.previewURL
        } onChange: {
            previewFinished.fulfill()
        }
        model.handle(.workspaceFileChunk(
            requestID: requestID,
            sessionID: "chat-1",
            path: file.path,
            offset: 0,
            data: Data([0, 1, 2]),
            nextOffset: nil
        ))
        await fulfillment(of: [previewFinished], timeout: 1)

        XCTAssertNotNil(model.previewURL)
        XCTAssertNil(model.textFilePreview)
        model.discardFilePresentation()
    }

    func testUnsupportedSessionFileCanBeDownloadedForSharing() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        let data = Data([0, 1, 2, 3])
        let file = SessionFileReference(
            id: "file-1",
            name: "report.xlsx",
            size: Int64(data.count),
            mediaType: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        )

        let requestCount = await recorder.requestCount()
        model.saveOrShareSessionFile(file)
        let request = await recorder.firstRequest(after: requestCount) { request in
            guard case .readSessionFile(_, _, let fileID, _, _) = request else { return false }
            return fileID == file.id
        }
        guard case .readSessionFile(let requestID, let sessionID, let fileID, let offset, _) = try XCTUnwrap(
            request
        )
        else { return XCTFail("Expected session file read request") }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(fileID, file.id)
        XCTAssertEqual(offset, 0)

        let shareFinished = expectation(description: "Share download finished")
        withObservationTracking {
            _ = model.sessionFileShareItem
        } onChange: {
            shareFinished.fulfill()
        }
        model.handle(.sessionFileChunk(
            requestID: requestID,
            sessionID: sessionID,
            fileID: fileID,
            offset: 0,
            data: data,
            nextOffset: nil
        ))
        await fulfillment(of: [shareFinished], timeout: 1)

        let shareItem = try XCTUnwrap(model.sessionFileShareItem)
        XCTAssertEqual(shareItem.name, file.name)
        XCTAssertEqual(shareItem.url.lastPathComponent, file.name)
        XCTAssertEqual(try Data(contentsOf: shareItem.url), data)
        XCTAssertNil(model.previewURL)
        XCTAssertNil(model.textFilePreview)
        model.discardFilePresentation()
    }

    func testTextEncodedImageSessionFileUsesQuickLookPreview() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        let data = Data("<svg/>".utf8)
        let file = SessionFileReference(
            id: "file-1",
            name: "diagram.svg",
            size: Int64(data.count),
            mediaType: "image/svg+xml"
        )

        let requestCount = await recorder.requestCount()
        model.previewSessionFile(file)
        let request = await recorder.firstRequest(after: requestCount) {
            guard case .readSessionFile(_, _, let fileID, _, _) = $0 else { return false }
            return fileID == file.id
        }
        guard case .readSessionFile(let requestID, _, _, _, _) = try XCTUnwrap(request)
        else { return XCTFail("Expected session file read request") }

        let previewFinished = expectation(description: "Image preview finished")
        withObservationTracking {
            _ = model.previewURL
        } onChange: {
            previewFinished.fulfill()
        }
        model.handle(.sessionFileChunk(
            requestID: requestID,
            sessionID: "chat-1",
            fileID: file.id,
            offset: 0,
            data: data,
            nextOffset: nil
        ))
        await fulfillment(of: [previewFinished], timeout: 1)

        XCTAssertEqual(model.previewURL?.pathExtension, "svg")
        XCTAssertNil(model.textFilePreview)
        model.discardFilePresentation()
    }

    func testStaleSessionFileChunkDoesNotCancelNewerDownload() async throws {
        let firstData = Data("first".utf8)
        let secondData = Data("second".utf8)
        let firstFile = SessionFileReference(
            id: "file-1",
            name: "first.txt",
            size: Int64(firstData.count),
            mediaType: "text/plain"
        )
        let secondFile = SessionFileReference(
            id: "file-2",
            name: "second.txt",
            size: Int64(secondData.count),
            mediaType: "text/plain"
        )
        let recorder = GatewayRequestRecorder()
        let firstReadSent = expectation(description: "First file read sent")
        let secondReadSent = expectation(description: "Second file read sent")
        let model = try model(requestSender: { request in
            await recorder.record(request)
            guard case .readSessionFile(_, _, let fileID, _, _) = request else { return }
            if fileID == firstFile.id { firstReadSent.fulfill() }
            if fileID == secondFile.id { secondReadSent.fulfill() }
        })
        model.selectedSessionID = "chat-1"

        model.previewSessionFile(firstFile)
        await fulfillment(of: [firstReadSent], timeout: 1)
        guard let firstRequest = await recorder.requests().last(where: {
            guard case .readSessionFile(_, _, let fileID, _, _) = $0 else { return false }
            return fileID == firstFile.id
        }), case .readSessionFile(let firstRequestID, _, _, _, _) = firstRequest
        else { return XCTFail("Expected first session file read") }

        model.previewSessionFile(secondFile)
        await fulfillment(of: [secondReadSent], timeout: 1)
        guard let secondRequest = await recorder.requests().last(where: {
            guard case .readSessionFile(_, _, let fileID, _, _) = $0 else { return false }
            return fileID == secondFile.id
        }), case .readSessionFile(let secondRequestID, _, _, _, _) = secondRequest
        else { return XCTFail("Expected second session file read") }
        XCTAssertNotEqual(firstRequestID, secondRequestID)

        model.handle(.sessionFileChunk(
            requestID: firstRequestID,
            sessionID: "chat-1",
            fileID: firstFile.id,
            offset: 0,
            data: firstData,
            nextOffset: nil
        ))
        XCTAssertTrue(model.isLoadingFilePresentation)
        XCTAssertNil(model.toast)

        let previewFinished = expectation(description: "Second file preview finished")
        withObservationTracking {
            _ = model.textFilePreview
        } onChange: {
            previewFinished.fulfill()
        }
        model.handle(.sessionFileChunk(
            requestID: secondRequestID,
            sessionID: "chat-1",
            fileID: secondFile.id,
            offset: 0,
            data: secondData,
            nextOffset: nil
        ))
        await fulfillment(of: [previewFinished], timeout: 1)

        XCTAssertEqual(model.textFilePreview?.name, secondFile.name)
        XCTAssertEqual(model.textFilePreview?.contents, "second")
        XCTAssertFalse(model.isLoadingFilePresentation)
        model.discardFilePresentation()
    }

    func testStaleWorkspaceFileChunkDoesNotCancelNewerDownload() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.selectedSessionID = "chat-1"
        let firstData = Data("first".utf8)
        let secondData = Data("second".utf8)
        let firstFile = WorkspaceFileRecord(path: "first.txt", size: UInt64(firstData.count))
        let secondFile = WorkspaceFileRecord(path: "second.txt", size: UInt64(secondData.count))

        model.previewWorkspaceFile(firstFile)
        try await Task.sleep(for: .milliseconds(20))
        guard let firstRequest = await recorder.requests().last(where: {
            if case .readWorkspaceFile = $0 { return true }
            return false
        }), case .readWorkspaceFile(let firstRequestID, _, _, _, _) = firstRequest
        else { return XCTFail("Expected first workspace file read") }

        model.previewWorkspaceFile(secondFile)
        try await Task.sleep(for: .milliseconds(20))
        guard let secondRequest = await recorder.requests().last(where: {
            if case .readWorkspaceFile = $0 { return true }
            return false
        }), case .readWorkspaceFile(let secondRequestID, _, _, _, _) = secondRequest
        else { return XCTFail("Expected second workspace file read") }
        XCTAssertNotEqual(firstRequestID, secondRequestID)

        model.handle(.workspaceFileChunk(
            requestID: firstRequestID,
            sessionID: "chat-1",
            path: firstFile.path,
            offset: 0,
            data: firstData,
            nextOffset: nil
        ))
        XCTAssertTrue(model.isLoadingFilePresentation)
        XCTAssertNil(model.toast)

        model.handle(.workspaceFileChunk(
            requestID: secondRequestID,
            sessionID: "chat-1",
            path: secondFile.path,
            offset: 0,
            data: secondData,
            nextOffset: nil
        ))
        try await Task.sleep(for: .milliseconds(20))

        XCTAssertEqual(model.textFilePreview?.name, "second.txt")
        XCTAssertEqual(model.textFilePreview?.contents, "second")
        XCTAssertFalse(model.isLoadingFilePresentation)
        model.discardFilePresentation()
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
        model.mountedWidgets = model.contributions.flatMap { contribution in
            contribution.widgets.map {
                MountedWidget(capability: contribution.capability, widget: $0)
            }
        }

        XCTAssertEqual(model.headerWidgets.first?.widget.text, "3 tasks")
        XCTAssertEqual(model.messageActionWidgets.first?.widget.text, "Fork chat")
        XCTAssertEqual(model.navigationWidgets.first?.id, "tasks\u{0}journal")
        XCTAssertEqual(model.chatMenuWidgets.first?.widget.text, "Open journal")
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
                "type": .string("user_message"),
                "message": .string("Fork here"),
                "attachments": .array([]),
                "messageTarget": .object([
                    "checkpointSequence": .number(12),
                    "batchItemCount": .number(3)
                ])
            ])),
            blocks: [],
            history: nil,
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

        let requestCount = await recorder.requestCount()
        model.submitMessageAction(widget, target: target)
        let request = await recorder.firstRequest(after: requestCount) {
            guard case .submit("chat-1", _) = $0 else { return false }
            return true
        }
        let requests = await recorder.requests()
        guard case .submit(let sessionID, let submission) = try XCTUnwrap(request),
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
        let events: [(JSONValue, [RenderedBlock])] = [
            (
                .object([
                    "type": .string("web_search_begin"),
                    "sessionId": .string("chat-1"),
                    "turnId": .string("turn-1"),
                    "modelStepId": .string("step-1"),
                    "callId": .string("search-1")
                ]),
                [RenderedBlock(capability: "web_search", block: FrontendBlock(
                    id: "step-1/search-1",
                    group: "turn-1",
                    update: .replace,
                    state: .pending,
                    role: .webSearch,
                    title: "Searching the web",
                    text: "",
                    symbol: "search",
                    format: "plain_text",
                    tone: "neutral",
                    files: []
                ))]
            ),
            (
                .object([
                    "type": .string("web_search_end"),
                    "sessionId": .string("chat-1"),
                    "turnId": .string("turn-1"),
                    "modelStepId": .string("step-1"),
                    "callId": .string("search-1"),
                    "action": .object([
                        "type": .string("search"),
                        "query": .string("Horus")
                    ])
                ]),
                [RenderedBlock(capability: "web_search", block: FrontendBlock(
                    id: "step-1/search-1",
                    group: "turn-1",
                    update: .replace,
                    state: .complete,
                    role: .webSearch,
                    title: "Searched the web",
                    text: "Horus",
                    symbol: "search",
                    format: "plain_text",
                    tone: "success",
                    files: []
                ))]
            ),
            (
                .object([
                    "type": .string("turn_aborted"),
                    "turnId": .string("turn-1"),
                    "reason": .string("Stopped")
                ]),
                [RenderedBlock(capability: "agent", block: FrontendBlock(
                    id: nil,
                    group: "turn-1",
                    update: .replace,
                    state: .complete,
                    role: .notice,
                    title: "Turn aborted",
                    text: "Stopped",
                    symbol: nil,
                    format: "plain_text",
                    tone: "warning",
                    files: []
                ))]
            ),
            (
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
                ]),
                []
            ),
        ]
        for (offset, item) in events.enumerated() {
            model.reduce(record: RecordedEvent(
                sequence: UInt64(offset + 1),
                recordedAtMs: Int64(1_000 + offset),
                event: AgentEventRecord(submissionId: nil, msg: item.0),
                streamMetrics: [],
                blocks: item.1,
                preview: nil
            ))
        }

        XCTAssertEqual(model.transcript.map(\.title), ["Searched the web", "Turn aborted"])
        XCTAssertEqual(model.transcript.map(\.text), ["Horus", "Stopped"])
        XCTAssertEqual(model.transcript.map(\.role), [.webSearch, .notice])
        XCTAssertEqual(model.transcript.map(\.tone), ["success", "warning"])
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
        model.applySessions([session(state: .idle, title: "Review")])
        model.selectedSessionID = "chat-1"
        model.setChatVisible(false)

        model.applySessions([session(state: .running, turnID: "turn-1", title: "Review")])
        model.applySessions([session(
            state: .idle,
            outcome: .failed,
            message: "Provider failed",
            title: "Review"
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
        XCTAssertTrue(model.transcript.isEmpty)
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
                enabled: ["skills"],
                settings: [
                    "context_offloading": ["stale_after_tokens": .integer(50_000)]
                ]
            ),
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

    func testHistoricalReplayAppearsOnlyWhenTheSnapshotIsComplete() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        let openRequestCount = await recorder.requestCount()
        model.openSession("chat-1")
        let openRequest = await recorder.firstRequest(after: openRequestCount) {
            guard case .openSession(_, "chat-1", nil) = $0 else { return false }
            return true
        }
        let request = try XCTUnwrap(openRequest)
        guard case .openSession(let requestID, _, nil) = request else {
            return XCTFail("Expected an uncached session open")
        }
        XCTAssertTrue(model.isLoadingTranscript)
        model.handle(.sessionOpened(requestID: requestID, payload: sessionReady(latestSequence: 2)))
        XCTAssertTrue(model.isLoadingTranscript)
        model.showFiles(.unstaged)
        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 1,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("agent_message_content_delta"),
                "modelStepId": .string("answer-1"),
                "phase": .string("final_answer"),
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
        XCTAssertEqual(model.transcript.map(\.text), ["Hello"])
        XCTAssertTrue(model.displayedTranscript.isEmpty)
        let refreshRequestCount = await recorder.requestCount()
        model.handle(.sessionReplayComplete(requestID: requestID, sessionID: "chat-1"))
        XCTAssertFalse(model.isLoadingTranscript)
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Hello"])
        let gitDiffRequest = await recorder.firstRequest(after: refreshRequestCount) { request in
            guard case .getGitDiff(_, "chat-1", .unstaged) = request else { return false }
            return true
        }
        let workspaceFilesRequest = await recorder.firstRequest(after: refreshRequestCount) { request in
            guard case .listWorkspaceFiles(_, "chat-1", .all) = request else { return false }
            return true
        }
        XCTAssertNotNil(gitDiffRequest)
        XCTAssertNotNil(workspaceFilesRequest)
    }

    func testEarlierHistoryUsesTheReadyCursorAndPrependsOnlyTranscriptState() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        let openRequestCount = await recorder.requestCount()
        model.openSession("chat-1")
        let openRequest = await recorder.firstRequest(after: openRequestCount) { request in
            guard case .openSession(_, "chat-1", _) = request else { return false }
            return true
        }
        guard case .openSession(let openID, _, _) = try XCTUnwrap(openRequest)
        else { return XCTFail("Expected session open") }
        model.handle(.sessionOpened(
            requestID: openID,
            payload: sessionReady(latestSequence: 8, nextBeforeSequence: 40)
        ))
        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 8,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("agent_message"),
                "sessionId": .string("chat-1"),
                "turnId": .string("turn-current"),
                "modelStepId": .string("step-current"),
                "phase": .string("final_answer"),
                "message": .string("Current"),
                "messageTarget": .null
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))
        model.handle(.sessionReplayComplete(requestID: openID, sessionID: "chat-1"))
        model.selectedModelRoute = "current-route"

        let initialRequests = await recorder.requests()
        let historyRequestCount = initialRequests.filter {
            if case .getSessionHistory = $0 { return true }
            return false
        }.count
        model.connectionState = .disconnected
        model.loadEarlierHistory()
        let disconnectedRequests = await recorder.requests()
        XCTAssertEqual(
            disconnectedRequests.filter {
                if case .getSessionHistory = $0 { return true }
                return false
            }.count,
            historyRequestCount
        )

        model.connectionState = .ready
        let readyRequestCount = await recorder.requestCount()
        model.loadEarlierHistory()
        model.loadEarlierHistory()
        let historyRequest = await recorder.firstRequest(after: readyRequestCount) { request in
            if case .getSessionHistory = request { return true }
            return false
        }
        let requests = await recorder.requests()
        XCTAssertEqual(
            requests.filter {
                if case .getSessionHistory = $0 { return true }
                return false
            }.count,
            1
        )
        guard case .getSessionHistory(
            let historyID,
            "chat-1",
            40,
            300
        ) = try XCTUnwrap(historyRequest) else {
            return XCTFail("Expected paged history request")
        }

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
                "phase": .string("commentary"),
                "message": .string("Earlier update")
            ]), blocks: []),
            RenderedEventRecord(event: .object([
                "type": .string("agent_message"),
                "message": .string("Older answer")
            ]), blocks: []),
        ]
        let records = events.enumerated().map { index, rendered in
            RecordedEvent(
                sequence: UInt64(index + 1),
                recordedAtMs: Int64(1_000 + index),
                event: AgentEventRecord(submissionId: nil, msg: rendered.event),
                streamMetrics: [],
                blocks: rendered.blocks,
                preview: nil
            )
        }
        model.handle(.sessionHistory(
            requestID: "stale",
            sessionID: "chat-1",
            records: records,
            nextBeforeSequence: nil
        ))
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Current"])

        model.handle(.sessionHistory(
            requestID: historyID,
            sessionID: "chat-1",
            records: records,
            nextBeforeSequence: nil
        ))

        XCTAssertEqual(
            model.displayedTranscript.map(\.text),
            ["Older question", "Earlier update", "Older answer", "Current"]
        )
        XCTAssertEqual(
            model.displayedTranscript.map(\.kind),
            [.user, .commentary, .assistant, .assistant]
        )
        XCTAssertEqual(model.selectedModelRoute, "current-route")
        XCTAssertFalse(model.hasEarlierHistory)

        model.restoreSession("chat-1")
        try await Task.sleep(for: .milliseconds(30))
        let reconnectRequests = await recorder.requests()
        guard case .openSession(let reconnectID, _, _) = try XCTUnwrap(
            reconnectRequests.last
        ) else { return XCTFail("Expected reconnect session open") }
        model.handle(.sessionOpened(
            requestID: reconnectID,
            payload: sessionReady(latestSequence: 8, nextBeforeSequence: 40)
        ))
        model.handle(.sessionReplayComplete(requestID: reconnectID, sessionID: "chat-1"))
        XCTAssertFalse(model.hasEarlierHistory)
    }

    func testHistoryPagesRebuildCrossPageAppendsAndFailedStepDeltas() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready

        let openRequestCount = await recorder.requestCount()
        model.openSession("chat-1")
        let open = await recorder.firstRequest(after: openRequestCount) {
            guard case .openSession(_, "chat-1", nil) = $0 else { return false }
            return true
        }
        guard case .openSession(let openID, _, _) = try XCTUnwrap(open) else {
            return XCTFail("Expected session open")
        }
        model.handle(.sessionOpened(
            requestID: openID,
            payload: sessionReady(latestSequence: 8, nextBeforeSequence: 7)
        ))

        let toolEnd = recorded(7, .object([
            "type": .string("tool_call_end"),
            "turnId": .string("turn-1"),
            "callId": .string("call-1"),
            "name": .string("shell"),
            "output": .string("Output"),
            "isError": .bool(false),
        ]), blocks: [RenderedBlock(capability: "tools", block: FrontendBlock(
            id: "turn-1/call-1",
            group: nil,
            update: .append,
            state: .complete,
            role: .tool,
            title: "Run command",
            text: "\nOutput",
            symbol: nil,
            format: "plain_text",
            tone: "success",
            files: []
        ))])
        let failed = recorded(8, .object([
            "type": .string("model_step_completed"),
            "sessionId": .string("chat-1"),
            "turnId": .string("turn-1"),
            "modelStepId": .string("step-1"),
            "stepIndex": .number(0),
            "startedAtMs": .number(100),
            "completedAtMs": .number(200),
            "outcome": .object(["status": .string("failed")]),
        ]))
        model.handle(.agentEvent(sessionID: "chat-1", record: toolEnd))
        model.handle(.agentEvent(sessionID: "chat-1", record: failed))
        model.handle(.sessionReplayComplete(requestID: openID, sessionID: "chat-1"))
        XCTAssertEqual(model.transcript.map(\.text), ["Output"])

        var requestCount = await recorder.requestCount()
        model.loadEarlierHistory()
        let firstHistory = await recorder.firstRequest(after: requestCount) {
            guard case .getSessionHistory = $0 else { return false }
            return true
        }
        guard case .getSessionHistory(let firstID, _, 7, _) = try XCTUnwrap(firstHistory) else {
            return XCTFail("Expected first history page")
        }
        let partial = recorded(6, .object([
            "type": .string("agent_reasoning_content_delta"),
            "sessionId": .string("chat-1"),
            "turnId": .string("turn-1"),
            "modelStepId": .string("step-1"),
            "delta": .string("Partial reasoning"),
        ]))
        model.handle(.sessionHistory(
            requestID: firstID,
            sessionID: "chat-1",
            records: [partial],
            nextBeforeSequence: 6
        ))
        XCTAssertEqual(model.transcript.map(\.text), ["Partial reasoning", "Output"])
        XCTAssertFalse(try XCTUnwrap(model.transcript.first).pending)

        requestCount = await recorder.requestCount()
        model.loadEarlierHistory()
        let secondHistory = await recorder.firstRequest(after: requestCount) {
            guard case .getSessionHistory = $0 else { return false }
            return true
        }
        guard case .getSessionHistory(let secondID, _, 6, _) = try XCTUnwrap(secondHistory) else {
            return XCTFail("Expected second history page")
        }
        let toolBegin = recorded(5, .object([
            "type": .string("tool_call_begin"),
            "turnId": .string("turn-1"),
            "callId": .string("call-1"),
            "name": .string("shell"),
            "arguments": .object(["cmd": .string("pwd")]),
        ]), blocks: [RenderedBlock(capability: "tools", block: FrontendBlock(
            id: "turn-1/call-1",
            group: nil,
            update: .replace,
            state: .pending,
            role: .tool,
            title: "Run command",
            text: "Arguments",
            symbol: nil,
            format: "plain_text",
            tone: "neutral",
            files: []
        ))])
        model.handle(.sessionHistory(
            requestID: secondID,
            sessionID: "chat-1",
            records: [toolBegin],
            nextBeforeSequence: nil
        ))

        XCTAssertEqual(model.transcript.map(\.text), ["Arguments\nOutput", "Partial reasoning"])
        XCTAssertEqual(model.transcript.map(\.role), [.tool, nil])
        XCTAssertTrue(model.transcript.allSatisfy { !$0.pending })
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
        guard case .openSession(let requestID, let sessionID, let cursor) = request else {
            return XCTFail("Expected a cached session open")
        }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(cursor, 7)
        XCTAssertTrue(model.isLoadingTranscript)

        model.handle(.sessionOpened(requestID: requestID, payload: sessionReady(latestSequence: 7)))
        XCTAssertEqual(model.transcript.map(\.text), ["Already rendered"])
        XCTAssertFalse(model.isLoadingTranscript)
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

    func testReconnectKeepsTheRetainedTranscriptVisibleWhileReplayLoads() throws {
        let model = try model()
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.transcript = [TranscriptEntry(
            id: "answer-1",
            text: "Already rendered",
            kind: .assistant,
            format: "plain_text",
            pending: false
        )]

        model.restoreSession("chat-1")

        XCTAssertEqual(model.connectionState, .loading)
        XCTAssertFalse(model.isLoadingTranscript)
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Already rendered"])
    }

    func testOpeningAnotherSessionStillHidesThePreviousTranscript() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model { request in await recorder.record(request) }
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        model.accounts = [account]
        model.selectedAccountID = account.id
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.transcript = [TranscriptEntry(
            id: "answer-1",
            text: "Previous chat",
            kind: .assistant,
            format: "plain_text",
            pending: false
        )]

        let requestCount = await recorder.requestCount()
        model.openSession("chat-2")
        let request = await recorder.firstRequest(after: requestCount) { request in
            guard case .openSession(_, "chat-2", _) = request else { return false }
            return true
        }

        XCTAssertNotNil(request)
        XCTAssertTrue(model.isLoadingTranscript)
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Previous chat"])
    }

    func testCanonicalReplayCompletesAfterTheFrozenCachedTranscript() async throws {
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
            sequence: 7,
            transcript: [TranscriptEntry(
                id: "model-stream:8:answer-1final_answer",
                text: "Cached",
                kind: .assistant,
                format: "plain_text",
                pending: false,
                modelStepID: "answer-1"
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
        guard case .openSession(let requestID, _, _) = try XCTUnwrap(requests.first) else {
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
                "modelStepId": .string("answer-2"),
                "phase": .string("final_answer"),
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
                "type": .string("agent_message"),
                "modelStepId": .string("answer-2"),
                "phase": .string("final_answer"),
                "message": .string("Canonical"),
                "messageTarget": .null
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))
        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Cached"])

        model.handle(.sessionReplayComplete(requestID: requestID, sessionID: "chat-1"))

        XCTAssertEqual(model.displayedTranscript.map(\.text), ["Cached", "Canonical"])
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
            sequence: 7,
            transcript: [TranscriptEntry(
                id: "model-stream:8:answer-1final_answer",
                text: "Cached",
                kind: .assistant,
                format: "plain_text",
                pending: false,
                modelStepID: "answer-1"
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
        guard case .openSession(let firstRequestID, _, _) = try XCTUnwrap(requests.first) else {
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
                "modelStepId": .string("answer-1"),
                "phase": .string("final_answer"),
                "delta": .string(" updated")
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))

        model.restoreSession("chat-1")
        try await Task.sleep(for: .milliseconds(20))
        requests = await recorder.requests()
        guard case .openSession(let secondRequestID, _, 8) = try XCTUnwrap(
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
                "modelStepId": .string("answer-1"),
                "phase": .string("final_answer"),
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
        #if !targetEnvironment(simulator)
        let attributes = try FileManager.default.attributesOfItem(atPath: XCTUnwrap(files.first).path)
        XCTAssertEqual(attributes[.protectionKey] as? FileProtectionType, .complete)
        #endif

        let oversizedAccountID = UUID()
        await store.saveTranscript(
            accountID: oversizedAccountID,
            sessionID: "large",
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

        var requestCount = await recorder.requestCount()
        model.openSession("chat-1")
        let firstRequest = await recorder.firstRequest(after: requestCount) { request in
            guard case .openSession(_, "chat-1", _) = request else { return false }
            return true
        }
        guard case .openSession(let firstID, "chat-1", _) = try XCTUnwrap(firstRequest)
        else { return XCTFail("Expected first session open") }
        model.composer = "Typed while opening"
        model.handle(.sessionOpened(
            requestID: firstID,
            payload: sessionReady(latestSequence: 1, sessionID: "chat-1")
        ))
        model.handle(.sessionReplayComplete(requestID: firstID, sessionID: "chat-1"))
        let firstSessionReady = await eventually { model.canCreateSession }
        XCTAssertTrue(firstSessionReady)
        XCTAssertEqual(model.composer, "Typed while opening")
        model.composer = "Draft one"

        requestCount = await recorder.requestCount()
        model.openSession("chat-2")
        let secondRequest = await recorder.firstRequest(after: requestCount) { request in
            guard case .openSession(_, "chat-2", _) = request else { return false }
            return true
        }
        let secondOpen = try XCTUnwrap(secondRequest)
        guard case .openSession(let secondID, _, _) = secondOpen else {
            return XCTFail("Expected second session open")
        }
        model.handle(.sessionOpened(
            requestID: secondID,
            payload: sessionReady(latestSequence: 1, sessionID: "chat-2")
        ))
        model.handle(.sessionReplayComplete(requestID: secondID, sessionID: "chat-2"))
        let secondSessionReady = await eventually {
            model.canCreateSession && model.composer == "Draft two"
        }
        XCTAssertTrue(secondSessionReady)

        let firstDraftSaved = await eventually {
            await store.loadComposerDraft(
                accountID: account.id,
                sessionID: "chat-1"
            ) == "Draft one"
        }
        XCTAssertTrue(firstDraftSaved)
        let firstDraft = await store.loadComposerDraft(
            accountID: account.id,
            sessionID: "chat-1"
        )
        XCTAssertEqual(firstDraft, "Draft one")
        XCTAssertEqual(model.composer, "Draft two")

        requestCount = await recorder.requestCount()
        model.sendMessage()
        model.composer = "Next draft"
        let submitRequest = await recorder.firstRequest(after: requestCount) { request in
            if case .submit = request { return true }
            return false
        }
        let submittedDraft = await store.loadComposerDraft(
            accountID: account.id,
            sessionID: "chat-2"
        )
        XCTAssertEqual(submittedDraft, "Draft two")
        guard case .submit(_, let submission) = try XCTUnwrap(submitRequest) else {
            return XCTFail("Expected submitted draft")
        }
        model.handle(.accepted(requestID: submission.id))
        let nextDraftSaved = await eventually {
            await store.loadComposerDraft(
                accountID: account.id,
                sessionID: "chat-2"
            ) == "Next draft"
        }
        XCTAssertTrue(nextDraftSaved)
        let nextDraft = await store.loadComposerDraft(
            accountID: account.id,
            sessionID: "chat-2"
        )
        XCTAssertEqual(nextDraft, "Next draft")

        requestCount = await recorder.requestCount()
        model.deleteSession(session(sessionID: "chat-2", state: .idle))
        let deleteRequest = await recorder.firstRequest(after: requestCount) { request in
            guard case .deleteSession(_, "chat-2") = request else { return false }
            return true
        }
        guard case .deleteSession(let deleteID, "chat-2") = try XCTUnwrap(deleteRequest)
        else { return XCTFail("Expected session delete") }
        model.handle(.accepted(requestID: deleteID))
        model.handle(.sessions(requestID: deleteID, sessions: []))
        let deletedDraftRemoved = await eventually {
            await store.loadComposerDraft(
                accountID: account.id,
                sessionID: "chat-2"
            ).isEmpty
        }
        XCTAssertTrue(deletedDraftRemoved)
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
        let firstRequest = await recorder.firstRequest(after: 0) { request in
            guard case .openSession(_, "chat-1", 7) = request else {
                return false
            }
            return true
        }
        let first = try XCTUnwrap(firstRequest)
        guard case .openSession(let requestID, _, 7) = first else {
            return XCTFail("Expected the cached cursor")
        }
        let requestCount = await recorder.requestCount()
        model.handle(.rejected(GatewayRejection(
            requestId: requestID,
            code: "replay_unavailable",
            message: "Reload",
            fatal: false
        )))
        let retryRequest = await recorder.firstRequest(after: requestCount) { request in
            guard case .openSession(_, "chat-1", nil) = request else { return false }
            return true
        }
        _ = try XCTUnwrap(retryRequest)

        let opens = await recorder.requests().compactMap { request -> (String, UInt64?)? in
            guard case .openSession(_, let sessionID, let cursor) = request else { return nil }
            return (sessionID, cursor)
        }
        XCTAssertEqual(opens.count, 2)
        XCTAssertEqual(opens.last?.0, "chat-1")
        XCTAssertNil(opens.last?.1)
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(1))
        var cached = await store.loadTranscript(accountID: account.id, sessionID: "chat-1")
        while cached != nil, clock.now < deadline {
            try await Task.sleep(for: .milliseconds(5))
            cached = await store.loadTranscript(accountID: account.id, sessionID: "chat-1")
        }
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
        XCTAssertTrue(model.transcript.isEmpty)
    }

    func testDelayedGeneratedTitleDoesNotOverwriteAnExplicitRename() async throws {
        let recorder = GatewayRequestRecorder()
        let writer = ChatTitleWriter { _ in
            try? await Task.sleep(for: .milliseconds(100))
            return "Generated title"
        }
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: writer
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)

        XCTAssertEqual(model.currentSessionTitle, "new conversation")

        try await submitMessage("Review the gateway", in: model, recorder: recorder)
        model.applySessions([session(
            state: .running,
            firstUserMessage: "Review the gateway",
            title: "Manual title"
        )])
        try await Task.sleep(for: .milliseconds(150))

        let requests = await recorder.requests()
        XCTAssertFalse(requests.contains {
            if case .renameSession = $0 { return true }
            return false
        })
    }

    func testGeneratedTitleAppearsAndPersistsAfterTheDurableUserMessage() async throws {
        let recorder = GatewayRequestRecorder()
        let writer = ChatTitleWriter { _ in "Generated title" }
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: writer
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)

        let submission = try await submitMessage(
            "Review the gateway",
            in: model,
            recorder: recorder
        )

        XCTAssertEqual(model.currentSessionTitle, "Generated title")
        let earlyRequests = await recorder.requests()
        XCTAssertFalse(earlyRequests.contains {
            if case .renameSession = $0 { return true }
            return false
        })

        model.reduce(
            event: AgentEventRecord(submissionId: submission.id, msg: .object([
                "type": .string("user_message"),
                "message": .string("Review the gateway")
            ])),
            blocks: [],
            preview: nil
        )
        try await Task.sleep(for: .milliseconds(30))

        let rename = await recorder.requests().first { request in
            guard case .renameSession(_, "chat-1", "Generated title") = request else {
                return false
            }
            return true
        }
        XCTAssertNotNil(rename)

        model.applySessions([session(
            state: .running,
            firstUserMessage: "Review the gateway",
            title: "Generated title"
        )])

        XCTAssertEqual(model.currentSessionTitle, "Generated title")
    }

    func testGeneratedTitlePersistsWhenTheCatalogTruncatesALongFirstMessage() async throws {
        let recorder = GatewayRequestRecorder()
        let writer = ChatTitleWriter { _ in "Generated title" }
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: writer
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)
        let prompt = String(repeating: "review this code carefully ", count: 30)

        try await submitMessage(prompt, in: model, recorder: recorder)
        model.applySessions([session(
            state: .running,
            firstUserMessage: String(decoding: prompt.utf8.prefix(512), as: UTF8.self)
        )])
        try await Task.sleep(for: .milliseconds(30))

        let rename = await recorder.requests().first { request in
            guard case .renameSession(_, "chat-1", "Generated title") = request else {
                return false
            }
            return true
        }
        XCTAssertNotNil(rename)
        XCTAssertEqual(model.currentSessionTitle, "Generated title")
    }

    func testGeneratedTitleWaitsForItsOwnDurableUserMessage() async throws {
        let recorder = GatewayRequestRecorder()
        let writer = ChatTitleWriter { _ in "Generated title" }
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: writer
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)
        let submission = try await submitMessage(
            "Review the gateway",
            in: model,
            recorder: recorder
        )

        model.reduce(
            event: AgentEventRecord(submissionId: "another-submission", msg: .object([
                "type": .string("user_message"),
                "message": .string("Another message")
            ])),
            blocks: [],
            preview: nil
        )
        try await Task.sleep(for: .milliseconds(30))
        let requestsAfterWrongMessage = await recorder.requests()
        XCTAssertFalse(requestsAfterWrongMessage.contains {
            if case .renameSession = $0 { return true }
            return false
        })

        model.reduce(
            event: AgentEventRecord(submissionId: submission.id, msg: .object([
                "type": .string("user_message"),
                "message": .string("Review the gateway")
            ])),
            blocks: [],
            preview: nil
        )
        try await Task.sleep(for: .milliseconds(30))

        let requestsAfterMatchingMessage = await recorder.requests()
        XCTAssertTrue(requestsAfterMatchingMessage.contains {
            guard case .renameSession(_, "chat-1", "Generated title") = $0 else {
                return false
            }
            return true
        })
    }

    func testGeneratedTitlePersistsWhenUserMessageArrivesBeforeGeneration() async throws {
        let recorder = GatewayRequestRecorder()
        let titleStarted = expectation(description: "Title generation started")
        let titleGate = AsyncGate()
        let writer = ChatTitleWriter { _ in
            titleStarted.fulfill()
            await titleGate.wait()
            return "Generated title"
        }
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: writer
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)
        let submission = try await submitMessage(
            "Review the gateway",
            in: model,
            recorder: recorder
        )
        await fulfillment(of: [titleStarted], timeout: 1)
        XCTAssertEqual(model.currentSessionTitle, "Review the gateway")

        let requestCount = await recorder.requestCount()
        model.reduce(
            event: AgentEventRecord(submissionId: submission.id, msg: .object([
                "type": .string("user_message"),
                "message": .string("Review the gateway")
            ])),
            blocks: [],
            preview: nil
        )
        await titleGate.open()

        let rename = await recorder.firstRequest(after: requestCount) { request in
            guard case .renameSession(_, "chat-1", "Generated title") = request else {
                return false
            }
            return true
        }
        XCTAssertNotNil(rename)
    }

    func testReplayedFirstMessagePersistsThePendingTitle() async throws {
        let recorder = GatewayRequestRecorder()
        let titleStarted = expectation(description: "Title generation started")
        let titleGate = AsyncGate()
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: ChatTitleWriter { _ in
                titleStarted.fulfill()
                await titleGate.wait()
                return "Generated title"
            }
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)
        try await submitMessage("Review the gateway", in: model, recorder: recorder)
        await fulfillment(of: [titleStarted], timeout: 1)

        let titleGenerated = expectation(description: "Generated title applied")
        withObservationTracking {
            _ = model.currentSessionTitle
        } onChange: {
            titleGenerated.fulfill()
        }
        await titleGate.open()
        await fulfillment(of: [titleGenerated], timeout: 1)
        XCTAssertEqual(model.currentSessionTitle, "Generated title")

        let requestCount = await recorder.requestCount()
        model.restoreSession("chat-1")
        let open = await recorder.firstRequest(after: requestCount) { request in
            guard case .openSession(_, "chat-1", _) = request else { return false }
            return true
        }
        guard case .openSession(let requestID, _, _) = try XCTUnwrap(open) else {
            return XCTFail("Expected the selected chat to reopen")
        }
        model.handle(.sessionOpened(
            requestID: requestID,
            payload: sessionReady(latestSequence: 1)
        ))
        model.handle(.agentEvent(
            sessionID: "chat-1",
            sequence: 1,
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("user_message"),
                "message": .string("Review the gateway"),
                "attachments": .array([]),
                "messageTarget": .object([
                    "checkpointSequence": .number(1),
                    "batchItemCount": .number(1)
                ])
            ])),
            blocks: [],
            history: nil,
            preview: nil
        ))
        let replayRequestCount = await recorder.requestCount()
        model.handle(.sessionReplayComplete(requestID: requestID, sessionID: "chat-1"))

        let rename = await recorder.firstRequest(after: replayRequestCount) { request in
            guard case .renameSession(_, "chat-1", "Generated title") = request else {
                return false
            }
            return true
        }
        XCTAssertNotNil(rename)
        XCTAssertEqual(model.currentSessionTitle, "Generated title")
    }

    func testReconnectRearmsTitleWhenTheSubmissionWasNotDurable() async throws {
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
        var prompts: [String] = []
        let firstTitleGenerated = expectation(description: "Initial title generated")
        let secondTitleGenerated = expectation(description: "Rearmed title generated")
        let model = AppModel(
            client: GatewayClient(),
            store: store,
            settingsDefaults: defaults,
            requestSender: { request in await recorder.record(request) },
            connectionOpener: { endpoint in try await harness.open(endpoint) },
            reconnectDelay: { _ in .zero },
            titleWriter: ChatTitleWriter { prompt in
                prompts.append(prompt)
                if prompts.count == 1 { firstTitleGenerated.fulfill() }
                if prompts.count == 2 { secondTitleGenerated.fulfill() }
                return "Generated title"
            }
        )
        await model.appDidBecomeActive()

        model.start()
        let initialConnectionOpened = await eventually { await harness.attemptCount() >= 2 }
        XCTAssertTrue(initialConnectionOpened)
        await harness.yield(.authenticated)
        await harness.yield(.ready(ready(
            defaultConfig: VersionedAgentConfig(revision: 1, config: composition()),
            sessions: []
        )))
        let gatewayReady = await eventually { model.connectionState.isReady }
        XCTAssertTrue(gatewayReady)
        try await openNewSession(in: model, recorder: recorder, account: account)
        try await submitMessage("Review the gateway", in: model, recorder: recorder)
        await fulfillment(of: [firstTitleGenerated], timeout: 1)
        XCTAssertEqual(model.currentSessionTitle, "Generated title")

        let reconnectRequestCount = await recorder.requestCount()
        await harness.fail()
        let reconnectOpened = await eventually { await harness.attemptCount() >= 3 }
        XCTAssertTrue(reconnectOpened)
        let attemptCount = await harness.attemptCount()
        XCTAssertGreaterThanOrEqual(attemptCount, 3)
        await harness.yield(.authenticated)
        await harness.yield(.ready(ready(
            defaultConfig: VersionedAgentConfig(revision: 1, config: composition()),
            sessions: [session(state: .idle, firstUserMessage: nil)]
        )))
        let reconnectRequest = await recorder.firstRequest(
            after: reconnectRequestCount
        ) { request in
            guard case .openSession(_, "chat-1", _) = request else { return false }
            return true
        }
        let reconnectOpen = try XCTUnwrap(reconnectRequest)
        guard case .openSession(let requestID, _, _) = reconnectOpen else {
            return XCTFail("Expected the selected chat to reopen")
        }
        await harness.yield(.sessionOpened(
            requestID: requestID,
            payload: sessionReady(latestSequence: 0)
        ))
        await harness.yield(.sessionReplayComplete(requestID: requestID, sessionID: "chat-1"))
        let replayFinished = await eventually { model.canCreateSession }
        XCTAssertTrue(replayFinished)
        XCTAssertEqual(model.currentSessionTitle, "new conversation")
        XCTAssertEqual(model.composer, "Review the gateway")
        try await submitMessage("Review the gateway", in: model, recorder: recorder)
        await fulfillment(of: [secondTitleGenerated], timeout: 1)
        XCTAssertEqual(prompts, ["Review the gateway", "Review the gateway"])
        XCTAssertEqual(model.currentSessionTitle, "Generated title")
    }

    func testRejectedFirstMessageRearmsAutomaticTitleGeneration() async throws {
        let recorder = GatewayRequestRecorder()
        var prompts: [String] = []
        let writer = ChatTitleWriter { prompt in
            prompts.append(prompt)
            return "Generated title"
        }
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: writer
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)

        let firstSubmission = try await submitMessage(
            "Review the gateway",
            in: model,
            recorder: recorder
        )
        XCTAssertEqual(model.currentSessionTitle, "Generated title")

        model.handle(.rejected(GatewayRejection(
            requestId: firstSubmission.id,
            code: "rejected",
            message: "Try again",
            fatal: false
        )))
        XCTAssertEqual(model.currentSessionTitle, "new conversation")
        XCTAssertEqual(model.composer, "Review the gateway")

        try await submitMessage("Review the gateway again", in: model, recorder: recorder)

        XCTAssertEqual(prompts, ["Review the gateway", "Review the gateway again"])
        XCTAssertEqual(model.currentSessionTitle, "Generated title")
    }

    func testRejectedFirstMessageRearmsAfterTitleGenerationReturnsNil() async throws {
        let recorder = GatewayRequestRecorder()
        var prompts: [String] = []
        let writer = ChatTitleWriter { prompt in
            prompts.append(prompt)
            return nil
        }
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: writer
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)

        let firstTitleFinished = expectation(description: "First title generation finished")
        withObservationTracking {
            _ = model.toast
        } onChange: {
            firstTitleFinished.fulfill()
        }
        let firstSubmission = try await submitMessage(
            "Review the gateway",
            in: model,
            recorder: recorder
        )
        await fulfillment(of: [firstTitleFinished], timeout: 1)

        model.handle(.rejected(GatewayRejection(
            requestId: firstSubmission.id,
            code: "rejected",
            message: "Try again",
            fatal: false
        )))
        let secondTitleFinished = expectation(description: "Second title generation finished")
        withObservationTracking {
            _ = model.toast
        } onChange: {
            secondTitleFinished.fulfill()
        }
        try await submitMessage("Review the gateway again", in: model, recorder: recorder)
        await fulfillment(of: [secondTitleFinished], timeout: 1)

        XCTAssertEqual(prompts, ["Review the gateway", "Review the gateway again"])
        XCTAssertEqual(model.currentSessionTitle, "Review the gateway again")
    }

    func testPromptPreviewRemainsWhenAppleDoesNotProduceATitle() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: ChatTitleWriter { _ in nil }
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)

        let submission = try await submitMessage(
            "Review the gateway retry behavior",
            in: model,
            recorder: recorder
        )
        XCTAssertEqual(model.currentSessionTitle, "Review the gateway retry behavior")
        XCTAssertEqual(model.toast?.message, "Apple did not produce a chat title.")
        XCTAssertEqual(model.toast?.tone, .warning)

        model.reduce(
            event: AgentEventRecord(submissionId: submission.id, msg: .object([
                "type": .string("user_message"),
                "message": .string("Review the gateway retry behavior")
            ])),
            blocks: [],
            preview: nil
        )
        model.applySessions([session(
            state: .running,
            firstUserMessage: "Review the gateway retry behavior"
        )])

        XCTAssertEqual(model.currentSessionTitle, "Review the gateway retry behavior")
        let requests = await recorder.requests()
        XCTAssertFalse(requests.contains {
            if case .renameSession = $0 { return true }
            return false
        })
    }

    func testAcceptedDeleteCancelsPendingTitleGeneration() async throws {
        let recorder = GatewayRequestRecorder()
        let deleteSent = expectation(description: "Delete request sent")
        let titleCancelled = expectation(description: "Title generation cancelled")
        let writer = ChatTitleWriter { _ in
            do {
                try await Task.sleep(for: .seconds(10))
            } catch is CancellationError {
                titleCancelled.fulfill()
            } catch {
                return nil
            }
            return "Generated title"
        }
        let model = try model(
            requestSender: { request in
                await recorder.record(request)
                if case .deleteSession = request { deleteSent.fulfill() }
            },
            titleWriter: writer
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(in: model, recorder: recorder, account: account)

        try await submitMessage("Review the gateway", in: model, recorder: recorder)
        let chat = session(state: .running, firstUserMessage: "Review the gateway")
        model.applySessions([chat])
        model.deleteSession(chat)
        await fulfillment(of: [deleteSent], timeout: 1)
        let deleteIDs = await recorder.requests().compactMap { request -> String? in
            guard case .deleteSession(let requestID, "chat-1") = request else { return nil }
            return requestID
        }
        let deleteID = try XCTUnwrap(deleteIDs.last)

        model.handle(.accepted(requestID: deleteID))
        await fulfillment(of: [titleCancelled], timeout: 1)

        let requests = await recorder.requests()
        XCTAssertFalse(requests.contains {
            if case .renameSession = $0 { return true }
            return false
        })
    }

    func testOpeningAnotherNewChatDoesNotCancelTheFirstTitle() async throws {
        let recorder = GatewayRequestRecorder()
        let firstTitleStarted = expectation(description: "First title generation started")
        let secondTitleStarted = expectation(description: "Second title generation started")
        let firstTitleGate = AsyncGate()
        let secondTitleGate = AsyncGate()
        let writer = ChatTitleWriter { prompt in
            if prompt == "First prompt" {
                firstTitleStarted.fulfill()
                await firstTitleGate.wait()
                return "First title"
            }
            secondTitleStarted.fulfill()
            await secondTitleGate.wait()
            return "Second title"
        }
        let model = try model(
            requestSender: { request in await recorder.record(request) },
            titleWriter: writer
        )
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        try await openNewSession(
            in: model,
            recorder: recorder,
            account: account,
            sessionID: "chat-1"
        )

        let firstSubmission = try await submitMessage(
            "First prompt",
            in: model,
            recorder: recorder,
            sessionID: "chat-1"
        )
        await fulfillment(of: [firstTitleStarted], timeout: 1)
        model.reduce(
            event: AgentEventRecord(submissionId: firstSubmission.id, msg: .object([
                "type": .string("user_message"),
                "message": .string("First prompt")
            ])),
            blocks: [],
            preview: nil
        )
        try await openNewSession(
            in: model,
            recorder: recorder,
            account: account,
            sessionID: "chat-2"
        )

        try await submitMessage(
            "Second prompt",
            in: model,
            recorder: recorder,
            sessionID: "chat-2"
        )
        await fulfillment(of: [secondTitleStarted], timeout: 1)

        let secondTitleGenerated = expectation(description: "Second title applied")
        withObservationTracking {
            _ = model.currentSessionTitle
        } onChange: {
            secondTitleGenerated.fulfill()
        }
        await secondTitleGate.open()
        await fulfillment(of: [secondTitleGenerated], timeout: 1)
        XCTAssertEqual(model.currentSessionTitle, "Second title")

        let requestCount = await recorder.requestCount()
        await firstTitleGate.open()
        let firstRename = await recorder.firstRequest(after: requestCount) { request in
            guard case .renameSession(_, "chat-1", "First title") = request else {
                return false
            }
            return true
        }
        XCTAssertNotNil(firstRename)

        XCTAssertEqual(
            model.displayedTitle(for: session(
                sessionID: "chat-1",
                state: .running,
                firstUserMessage: "First prompt"
            )),
            "First title"
        )
    }
}

final class ChatTitleWriterTests: XCTestCase {
    func testBuildsACompactPromptPreview() {
        XCTAssertEqual(
            ChatTitleWriter.preview(for: "  Review\n the   gateway retry behavior"),
            "Review the gateway retry behavior"
        )
        XCTAssertEqual(
            ChatTitleWriter.preview(for: String(repeating: "a", count: 42) + "   \n"),
            String(repeating: "a", count: 42)
        )
        XCTAssertEqual(
            ChatTitleWriter.preview(for: String(repeating: "🧪", count: 43)),
            String(repeating: "🧪", count: 42) + "…"
        )
        XCTAssertNil(ChatTitleWriter.preview(for: " \n "))
        XCTAssertNil(ChatTitleWriter.preview(for: nil))
    }

    func testStripsTheDressingSmallModelsAddToTitles() {
        XCTAssertEqual(ChatTitleWriter.cleaned("\"Fix the retry backoff\""), "Fix the retry backoff")
        XCTAssertEqual(ChatTitleWriter.cleaned("Title: Rename the gateway"), "Rename the gateway")
        XCTAssertEqual(ChatTitleWriter.cleaned("Title:\nUseful title"), "Useful title")
        XCTAssertEqual(ChatTitleWriter.cleaned("Audit the sandbox policy."), "Audit the sandbox policy")
        XCTAssertEqual(ChatTitleWriter.cleaned("  Trim whitespace \n and drop the rest"), "Trim whitespace")
    }

    func testRejectsOnlyEmptyOutputAndFitsVerboseTitles() {
        XCTAssertNil(ChatTitleWriter.cleaned(""))
        XCTAssertNil(ChatTitleWriter.cleaned("   \n  "))
        XCTAssertNil(ChatTitleWriter.cleaned("\"\""))
        XCTAssertEqual(ChatTitleWriter.cleaned("One two three four five"), "One two three four")
        XCTAssertEqual(
            ChatTitleWriter.cleaned("Extraordinarilylongword anotherlongword useful title"),
            "Extraordinarilylongword anotherlongword…"
        )
    }

    @MainActor
    func testInjectedGeneratorUsesTheProductionValidationPath() async {
        let writer = ChatTitleWriter { _ in "One two three four five" }

        guard case .title(let title) = await writer.title(for: "Review the gateway") else {
            return XCTFail("Expected the generated title to be fitted")
        }
        XCTAssertEqual(title, "One two three four")
    }
}

final class TranscriptEventLineTests: XCTestCase {
    private func entry(
        id: String,
        text: String,
        kind: TranscriptEntry.Kind = .event,
        tone: String = "neutral",
        format: String = "plain_text",
        capability: String? = nil,
        role: FrontendBlockRole? = nil,
        title: String = "",
        group: String? = nil
    ) -> TranscriptEntry {
        TranscriptEntry(
            id: id,
            text: text,
            kind: kind,
            capability: capability,
            role: role,
            title: title,
            group: group,
            format: format,
            tone: tone,
            pending: false
        )
    }

    func testUsesTypedPresentationWithoutParsingIDOrProse() {
        let call = entry(
            id: "misleading/legacy/id",
            text: "◉ This remains body text\ntotal 8",
            capability: "tools",
            role: .tool,
            title: "Run command"
        )

        XCTAssertEqual(call.capability, "tools")
        XCTAssertEqual(call.headline, "Run command")
        XCTAssertEqual(call.eventDetail, "◉ This remains body text\ntotal 8")
    }

    func testDoesNotInferMissingMetadata() {
        let bare = entry(id: "9C4F-2B", text: "")

        XCTAssertNil(bare.capability)
        XCTAssertNil(bare.role)
        XCTAssertEqual(bare.headline, "")
        XCTAssertEqual(bare.eventDetail, "")
    }

    func testSummaryCountsAndPluralisesByCategory() {
        let entries = [
            entry(id: "a", text: "", role: .tool),
            entry(id: "b", text: "", role: .tool),
            entry(id: "c", text: "", role: .activity),
            entry(id: "d", text: "", role: .webSearch),
            entry(id: "e", text: "", kind: .error, tone: "error", role: .tool)
        ]

        XCTAssertEqual(
            TranscriptEntry.summary(for: entries),
            "2 tool calls • 1 web search • 1 event • 1 error"
        )
        XCTAssertEqual(TranscriptEntry.summary(for: [entries[0]]), "1 tool call")
        // "search" takes -es, which a bare +"s" would get wrong.
        XCTAssertEqual(
            TranscriptEntry.summary(for: [entries[3], entries[3]]),
            "2 web searches"
        )
    }

    func testGroupsSequentialMixedActivityAcrossCapabilitiesAndGroups() {
        let tool = entry(
            id: "tool",
            text: "Read a file",
            capability: "tools",
            role: .tool,
            group: "tools/turn"
        )
        let event = entry(
            id: "event",
            text: "Delegated a task",
            capability: "subagents",
            role: .activity,
            group: "subagents/turn"
        )
        let search = entry(
            id: "search",
            text: "Searched the web",
            capability: "web_search",
            role: .webSearch,
            group: "web_search/turn"
        )
        let reasoning = entry(id: "reasoning", text: "Thinking", kind: .reasoning)
        let laterTool = entry(id: "later-tool", text: "Checked again", role: .tool)
        let notice = entry(id: "notice", text: "Needs attention", role: .notice)

        let rows = TranscriptEntry.groupedRows(
            from: [tool, event, search, reasoning, laterTool, notice]
        )
        XCTAssertEqual(
            rows.map { $0.map(\.id) },
            [["tool", "event", "search"], ["reasoning"], ["later-tool"], ["notice"]]
        )

        XCTAssertEqual(
            TranscriptEntry.groupedRows(
                from: [tool, event],
                breakBefore: event.id
            ).map { $0.map(\.id) },
            [["tool"], ["event"]]
        )
    }

    func testWebSearchUsesOnlyTheTypedRole() {
        XCTAssertTrue(entry(id: "anything", text: "not search prose", role: .webSearch).isWebSearch)
        XCTAssertFalse(entry(
            id: "web_search/deceptive",
            text: "Search the web",
            capability: "web_search",
            role: .tool
        ).isWebSearch)
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
