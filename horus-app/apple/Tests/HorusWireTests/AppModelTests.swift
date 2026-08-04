import Foundation
import XCTest

@MainActor
final class AppModelTests: XCTestCase {
    private func model() throws -> AppModel {
        let suiteName = UUID().uuidString
        return AppModel(
            client: GatewayClient(),
            store: GatewayStore(defaults: try XCTUnwrap(UserDefaults(suiteName: suiteName)))
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

    func testSwitchingGatewaysClearsGatewayScopedStateBeforeTokenLookup() throws {
        let suiteName = UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let store = GatewayStore(defaults: defaults)
        let first = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        let second = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9192"))
        try store.save(first, token: "first-token")
        defer { try? store.remove(first) }

        let model = AppModel(client: GatewayClient(), store: store)
        model.accounts.append(second)
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
            capability: "subagents",
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
        XCTAssertEqual(snapshot.blocks.last?.block.id, "subagents/change")
        XCTAssertEqual(snapshot.blocks.last?.block.group, "subagents/work")
        XCTAssertEqual(snapshot.blocks.last?.block.format, "unified_diff")
        XCTAssertEqual(snapshot.blocks.last?.block.tone, "success")
    }

    func testContributionCatalogReferencesAndHeaderWidgetsAreGeneric() throws {
        let model = try model()
        model.contributions = [FrontendContribution(
            capability: "tasks",
            commands: [FrontendCommand(
                name: "tasks",
                arguments: "[filter]",
                description: "List tasks"
            )],
            widgets: [FrontendWidget(
                id: "count",
                slot: "header",
                text: "3 tasks",
                tone: "success",
                action: nil
            )],
            references: [FrontendReference(trigger: "$", value: "planning", description: "Planning skill")],
            activeInput: nil
        )]
        model.mountedWidgets = model.contributions.flatMap { contribution in
            contribution.widgets.map {
                MountedWidget(capability: contribution.capability, widget: $0)
            }
        }

        XCTAssertEqual(model.capabilityCommands.first?.command.name, "tasks")
        XCTAssertEqual(model.headerWidgets.first?.widget.text, "3 tasks")
        let text = "Use $plan"
        let suggestions = try XCTUnwrap(model.referenceSuggestions(in: text, cursor: text.endIndex))
        XCTAssertEqual(String(text[suggestions.range]), "$plan")
        XCTAssertEqual(suggestions.matches.first?.replacement, "$planning")
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
                        "totalTokens": .number(1_100)
                    ]),
                    "lastTokenUsage": .object([
                        "inputTokens": .number(40),
                        "cachedInputTokens": .number(20),
                        "outputTokens": .number(10),
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
        XCTAssertEqual(model.contextTokens, 99)
        XCTAssertEqual(model.modelContextWindow, 200)
    }

    func testSessionActivityShowsOnlyUnseenCompletion() throws {
        let model = try model()
        model.selectedSessionID = "chat-1"
        model.destination = .agent
        model.setChatVisible(false)

        model.updateSessionActivity(
            sessionID: "chat-1",
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("task_started"),
                "turnId": .string("turn-1")
            ]))
        )
        XCTAssertTrue(model.runningSessionIDs.contains("chat-1"))

        model.updateSessionActivity(
            sessionID: "chat-1",
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("exec_approval_request"),
                "id": .string("approval-1")
            ]))
        )
        XCTAssertEqual(model.toast?.tone, .warning)

        model.updateSessionActivity(
            sessionID: "chat-1",
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("task_complete"),
                "turnId": .string("turn-1")
            ]))
        )
        XCTAssertFalse(model.runningSessionIDs.contains("chat-1"))
        XCTAssertTrue(model.unreadSessionIDs.contains("chat-1"))
        XCTAssertEqual(model.toast?.tone, .success)

        model.destination = .chat
        model.setChatVisible(true)
        XCTAssertFalse(model.unreadSessionIDs.contains("chat-1"))
        model.dismissToast()
        model.updateSessionActivity(
            sessionID: "chat-1",
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("task_complete"),
                "turnId": .string("turn-2")
            ]))
        )
        XCTAssertNil(model.toast)
    }

    func testAgentErrorOutranksAbortWithoutDroppingLaterFeedback() throws {
        let model = try model()
        model.selectedSessionID = "chat-1"
        model.setChatVisible(false)

        model.updateSessionActivity(
            sessionID: "chat-1",
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("error"),
                "message": .string("Provider failed")
            ]))
        )
        model.updateSessionActivity(
            sessionID: "chat-1",
            event: AgentEventRecord(submissionId: nil, msg: .object([
                "type": .string("turn_aborted"),
                "turnId": .string("turn-1")
            ]))
        )

        XCTAssertEqual(model.toast?.message, "Provider failed")
        XCTAssertEqual(model.toast?.tone, .error)
        XCTAssertTrue(model.unreadSessionIDs.contains("chat-1"))

        model.showToast("Credential saved.", tone: .success)
        XCTAssertEqual(model.toast?.message, "Credential saved.")
        XCTAssertEqual(model.toast?.tone, .success)
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
