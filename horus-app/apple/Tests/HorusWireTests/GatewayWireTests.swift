import Foundation
import XCTest

final class GatewayWireTests: XCTestCase {
    private func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }

    private func encoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }

    func testPairedFixtureDecodesConvertedIdentifier() throws {
        let fixture = #"{"version":1,"type":"paired","client_id":"phone-7","token":"bearer"}"#
        let envelope = try decoder().decode(GatewayEnvelope.self, from: Data(fixture.utf8))

        guard case .paired(let clientID, let token) = envelope else {
            return XCTFail("Expected paired envelope")
        }
        XCTAssertEqual(clientID, "phone-7")
        XCTAssertEqual(token, "bearer")
    }

    func testAgentEventFixtureConvertsNestedEventKeys() throws {
        let fixture = #"{"version":1,"type":"agent_event","sequence":7,"event":{"submission_id":"input-1","msg":{"type":"session_history","events":[]}},"blocks":[],"history":[{"event":{"type":"frontend","frontend_type":"render","capability":"tools","block":{"id":"result-1","group":null,"append":false,"pending":false,"text":"Hello","format":"plain_text","tone":"neutral"}},"blocks":[{"id":"tools/result-1","group":null,"append":false,"pending":false,"text":"Hello","format":"plain_text","tone":"neutral"}]}],"preview":null}"#
        let envelope = try decoder().decode(GatewayEnvelope.self, from: Data(fixture.utf8))

        guard case .agentEvent(let sequence, let event, let blocks, let history, let preview) = envelope else {
            return XCTFail("Expected agent event envelope")
        }
        XCTAssertEqual(sequence, 7)
        XCTAssertEqual(event.submissionId, "input-1")
        XCTAssertEqual(event.msg["type"]?.stringValue, "session_history")
        XCTAssertTrue(blocks.isEmpty)
        XCTAssertEqual(history?.first?.event["frontendType"]?.stringValue, "render")
        XCTAssertEqual(history?.first?.blocks.first?.id, "tools/result-1")
        XCTAssertNil(preview)
    }

    func testOperationFixtureConvertsNestedIdentifiers() throws {
        let fixture = #"{"type":"active_input","operation":"steer","turn_id":"turn-2","text":"Focus here"}"#
        let operation = try decoder().decode(AgentOperation.self, from: Data(fixture.utf8))

        guard case .activeInput(let name, let turnID, let text) = operation else {
            return XCTFail("Expected active input operation")
        }
        XCTAssertEqual(name, "steer")
        XCTAssertEqual(turnID, "turn-2")
        XCTAssertEqual(text, "Focus here")
    }

    func testConfigureAgentEncodingMatchesGatewayFixture() throws {
        let config = AgentComposition(
            provider: ProviderSelection(
                provider: "openai",
                model: "gpt-5",
                baseUrl: nil,
                apiKeyEnv: "OPENAI_API_KEY",
                reasoningEffort: "high",
                webSearch: "cached"
            ),
            middleware: MiddlewareSelection(
                tools: true,
                skills: true,
                subagents: true,
                steering: true,
                compaction: true,
                sessions: true
            ),
            approval: .on,
            systemPrompt: "Stay focused."
        )
        let request = GatewayRequest.configureAgent(
            requestID: "config-9",
            expectedRevision: 4,
            config: config
        )

        let data = try encoder().encode(request)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(object["type"] as? String, "configure_agent")
        XCTAssertEqual(object["request_id"] as? String, "config-9")
        XCTAssertEqual(object["expected_revision"] as? Int, 4)
        XCTAssertNil(object["requestId"])

        let encodedConfig = try XCTUnwrap(object["config"] as? [String: Any])
        XCTAssertEqual(encodedConfig["system_prompt"] as? String, "Stay focused.")
        let provider = try XCTUnwrap(encodedConfig["provider"] as? [String: Any])
        XCTAssertEqual(provider["api_key_env"] as? String, "OPENAI_API_KEY")
        XCTAssertEqual(provider["reasoning_effort"] as? String, "high")
        XCTAssertNil(provider["base_url"])
    }

    func testPlaintextEndpointIsRestrictedToLoopback() throws {
        XCTAssertEqual(try GatewayEndpoint("tcp://localhost:9191").rawValue, "tcp://localhost:9191")
        XCTAssertThrowsError(try GatewayEndpoint("tcp://example.com:9191")) { error in
            XCTAssertEqual(error as? GatewayWireError, .insecureRemoteEndpoint)
        }
        XCTAssertEqual(try GatewayEndpoint("tls://example.com:443").rawValue, "tls://example.com:443")
    }

    func testEndpointValidationAlsoAppliesWhenDecoding() throws {
        let fixture = #"{"rawValue":"tcp://example.com:9191"}"#

        XCTAssertThrowsError(try decoder().decode(GatewayEndpoint.self, from: Data(fixture.utf8))) { error in
            XCTAssertEqual(error as? GatewayWireError, .insecureRemoteEndpoint)
        }
    }

    func testEndpointValidatesHostAndNormalizesIPv6() throws {
        XCTAssertThrowsError(try GatewayEndpoint("tls://:443"))
        XCTAssertEqual(try GatewayEndpoint("tcp://[::1]:9191").rawValue, "tcp://[::1]:9191")
        XCTAssertEqual(try GatewayEndpoint("tls://[2001:db8::1]:443").host, "2001:db8::1")
    }

    @MainActor
    func testMissingGatewayTokenRequiresRepair() throws {
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        let defaults = try XCTUnwrap(UserDefaults(suiteName: UUID().uuidString))
        let store = GatewayStore(defaults: defaults)

        XCTAssertThrowsError(try store.token(for: account)) { error in
            guard let storeError = error as? GatewayStore.StoreError,
                  case .missingToken = storeError
            else { return XCTFail("Expected a missing-token error") }
        }
    }

    @MainActor
    func testReplacingGatewayTokenKeepsAccountUsable() throws {
        let account = GatewayAccount(endpoint: try GatewayEndpoint("tcp://localhost:9191"))
        let defaults = try XCTUnwrap(UserDefaults(suiteName: UUID().uuidString))
        let store = GatewayStore(defaults: defaults)
        defer { try? store.remove(account) }

        try store.save(account, token: "first")
        try store.save(account, token: "second")

        XCTAssertEqual(try store.token(for: account), "second")
    }

    func testSetWorkspaceEncodingUsesGatewayHostPath() throws {
        let data = try encoder().encode(GatewayRequest.setWorkspace(
            requestID: "workspace-3",
            path: "/srv/horus/project"
        ))
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])

        XCTAssertEqual(object["type"] as? String, "set_workspace")
        XCTAssertEqual(object["request_id"] as? String, "workspace-3")
        XCTAssertEqual(object["path"] as? String, "/srv/horus/project")
    }

    func testSessionAndBranchRequestsMatchGatewayWire() throws {
        let requests: [(GatewayRequest, String)] = [
            (.renameSession(requestID: "rename-1", sessionID: "chat-1", title: "Review"), "rename_session"),
            (.setSessionPinned(requestID: "pin-1", sessionID: "chat-1", pinned: true), "set_session_pinned"),
            (.deleteSession(requestID: "delete-1", sessionID: "chat-1"), "delete_session"),
            (.setGitBranch(requestID: "branch-1", branch: "feature/ui"), "set_git_branch"),
            (.getGitDiff(requestID: "diff-1"), "get_git_diff")
        ]

        for (request, type) in requests {
            let data = try encoder().encode(request)
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
            XCTAssertEqual(object["type"] as? String, type)
        }
    }

    func testGitDiffEnvelopeDecodesWorkspaceDiff() throws {
        let fixture = #"{"version":1,"type":"git_diff","request_id":"diff-1","diff":"diff --git a/a.swift b/a.swift\n"}"#
        let envelope = try decoder().decode(GatewayEnvelope.self, from: Data(fixture.utf8))

        guard case .gitDiff(let requestID, let diff) = envelope else {
            return XCTFail("Expected git diff envelope")
        }
        XCTAssertEqual(requestID, "diff-1")
        XCTAssertTrue(diff.hasPrefix("diff --git"))
    }

    func testDirectoryListingRoundTripUsesGatewayPaths() throws {
        let request = try encoder().encode(GatewayRequest.listDirectories(
            requestID: "directories-4",
            path: "/srv/horus",
            includeFiles: true
        ))
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: request) as? [String: Any])
        XCTAssertEqual(object["type"] as? String, "list_directories")
        XCTAssertEqual(object["path"] as? String, "/srv/horus")
        XCTAssertEqual(object["include_files"] as? Bool, true)

        let fixture = #"{"version":1,"type":"directories","request_id":"directories-4","listing":{"path":"/srv/horus","parent":"/srv","entries":[{"name":"project","path":"/srv/horus/project","is_directory":true}]}}"#
        let envelope = try decoder().decode(GatewayEnvelope.self, from: Data(fixture.utf8))
        guard case .directories(let requestID, let listing) = envelope else {
            return XCTFail("Expected directories envelope")
        }
        XCTAssertEqual(requestID, "directories-4")
        XCTAssertEqual(listing.entries.first?.name, "project")
        XCTAssertEqual(listing.entries.first?.isDirectory, true)
    }

    func testProviderStatusDecodesAdvertisedDefaults() throws {
        let fixture = #"{"provider":"openai_socket","label":"OpenAI (API key)","configured":true,"auth":"api_key","default_model":"gpt-5.6-sol","default_base_url":null,"default_api_key_env":"OPENAI_API_KEY","default_reasoning_effort":"medium","default_web_search":"off"}"#
        let status = try decoder().decode(ProviderStatus.self, from: Data(fixture.utf8))

        XCTAssertEqual(status.label, "OpenAI (API key)")
        XCTAssertEqual(status.defaultModel, "gpt-5.6-sol")
        XCTAssertNil(status.defaultBaseUrl)
        XCTAssertEqual(status.defaultReasoningEffort, "medium")
        XCTAssertEqual(status.defaultWebSearch, "off")
    }

    func testPreviewTextFallsBackToCoreMessages() {
        let event = RenderedEventRecord(
            event: .object([
                "type": .string("agent_message"),
                "message": .string("Finished the review.")
            ]),
            blocks: []
        )

        XCTAssertEqual(event.previewText, ["Horus\nFinished the review."])
    }

    func testSameRevisionRefreshPreservesProviderDraft() {
        let configured = AgentComposition(
            provider: ProviderSelection(
                provider: "openai_codex",
                model: "gpt-5.6-sol",
                baseUrl: nil,
                apiKeyEnv: nil,
                reasoningEffort: "medium",
                webSearch: "off"
            ),
            middleware: MiddlewareSelection(
                tools: true,
                skills: true,
                subagents: true,
                steering: true,
                compaction: true,
                sessions: true
            ),
            approval: .on,
            systemPrompt: "Stay focused."
        )
        let snapshot = VersionedAgentConfig(revision: 4, config: configured)
        var draft = configured
        draft.provider.provider = "openrouter"

        let refreshed = refreshedAgentDraft(
            currentDraft: draft,
            currentSnapshot: snapshot,
            incomingSnapshot: snapshot
        )

        XCTAssertEqual(refreshed.provider.provider, "openrouter")
    }
}
