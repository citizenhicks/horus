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

    private func requestObject(_ request: GatewayRequest) throws -> [String: Any] {
        let data = try encoder().encode(request)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(object["version"] as? Int, gatewayProtocolVersion)
        return object
    }

    private func decodeEnvelope(_ fixture: String) throws -> GatewayEnvelope {
        try decoder().decode(GatewayEnvelope.self, from: Data(fixture.utf8))
    }

    private var configJSON: String {
        #"{"revision":4,"config":{"provider":{"provider":"openai_socket","model":"gpt-5.6-sol","reasoning_effort":"high","web_search":"cached"},"middleware":{"enabled":["skills","subagents","tools"],"settings":{"context_offloading":{"stale_after_tokens":50000},"subagents":{"model_route":"openai_socket/gpt-5.6-sol"}}},"approval":"on","system_prompt":"Stay focused."}}"#
    }

    private var sessionRecordJSON: String {
        #"{"session_id":"chat-1","session_context":{"workspace_id":"workspace-1"},"parent_session_id":null,"parent_sequence":null,"sequence":7,"catalog_visible":true,"first_user_message":"Review this","created_at":100,"updated_at":200,"title":"Review","pinned":true,"activity":{"state":"running","turn_id":"turn-1","started_at":190,"last_outcome":null,"message":null}}"#
    }

    private var readyPayloadJSON: String {
        #"{"sessions":[\#(sessionRecordJSON)],"providers":[{"provider":"openai_socket","label":"OpenAI","symbol":"sparkle","description":"Persistent Responses API","configured":true,"auth":"api_key","default_base_url":null,"default_api_key_env":"OPENAI_API_KEY","models":[{"id":"gpt-5.6-sol","label":"Sol","description":"Frontier capability for complex work","context_window":1050000,"reasoning":[{"id":"medium","label":"Medium","description":"Balanced reasoning and latency"}],"default_reasoning":"medium"}],"web_search":["off","cached","live"]}],"default_config":\#(configJSON),"models":[{"route":"openai_socket/gpt-5.6-sol","group":"OpenAI","model":"gpt-5.6-sol","reasoning_effort":"medium","context_window":200000}],"middleware_features":[{"id":"skills","label":"Skills","description":"Load focused instructions.","required":false,"settings":[{"id":"limit","label":"Limit","description":"Maximum items","type":"integer","min":1,"step":10},{"id":"route","label":"Route","description":"Default route","type":"select","options":[{"value":"route-a","label":"Route A","description":"First route"}],"unset_label":"Inherit"}]}],"max_active_sessions":4}"#
    }

    private var sessionReadyPayloadJSON: String {
        #"{"latest_sequence":7,"workspace":{"id":"workspace-1","path":"/srv/horus"},"git":{"current_branch":"main","branches":["feature","main"]},"session":{"session_id":"chat-1","context":{"workspace_id":"workspace-1"},"model":{"route":"openai_socket/gpt-5.6-sol","model":"gpt-5.6-sol","reasoning_effort":"high","model_context_window":200000}},"contributions":[{"capability":"subagents","count":2,"commands":[],"widgets":[{"id":"subagents","slot":"composer_footer","text":"Subagents","tone":"neutral","symbol":"robot","icon_only":true,"progress":null,"content":{"type":"picker","title":"Subagents","options":[{"label":"reviewer","description":"running","detail":"gpt-5.6-sol","op":{"type":"capability_command","capability":"subagents","command":"subagents","arguments":"reviewer","target":null}}]},"action":null}],"references":[],"active_input":null}],"tool_count":7,"config":\#(configJSON)}"#
    }

    private var composition: AgentComposition {
        AgentComposition(
            provider: ProviderConfig(
                provider: "openai_socket",
                model: "gpt-5.6-sol",
                baseUrl: nil,
                reasoningEffort: "high",
                webSearch: .cached
            ),
            middleware: MiddlewareConfig(
                enabled: ["skills", "subagents", "tools"],
                settings: [
                    "context_offloading": ["stale_after_tokens": .integer(50_000)],
                    "subagents": ["model_route": .string("openai_socket/gpt-5.6-sol")]
                ]
            ),
            approval: .on,
            systemPrompt: "Stay focused."
        )
    }

    func testProtocolV8PairAndAuthenticateRequireClientKind() throws {
        let pair = try requestObject(.pair(
            code: "123456",
            clientLabel: "Phone",
            clientKind: .ios
        ))
        XCTAssertEqual(pair["type"] as? String, "pair")
        XCTAssertEqual(pair["client_label"] as? String, "Phone")
        XCTAssertEqual(pair["client_kind"] as? String, "ios")
        XCTAssertNil(pair["last_sequence"])

        let authenticate = try requestObject(.authenticate(token: "bearer", clientKind: .macos))
        XCTAssertEqual(authenticate["type"] as? String, "authenticate")
        XCTAssertEqual(authenticate["token"] as? String, "bearer")
        XCTAssertEqual(authenticate["client_kind"] as? String, "macos")
        XCTAssertNil(authenticate["last_sequence"])
    }

    func testProtocolV8ClientInventoryRoundTrip() throws {
        let request = try requestObject(.listClients(requestID: "clients-1"))
        XCTAssertEqual(request["type"] as? String, "list_clients")
        XCTAssertEqual(request["request_id"] as? String, "clients-1")
        let unpair = try requestObject(.unpairClient(
            requestID: "unpair-1",
            clientID: "phone-7"
        ))
        XCTAssertEqual(unpair["type"] as? String, "unpair_client")
        XCTAssertEqual(unpair["client_id"] as? String, "phone-7")

        let fixture = #"{"version":8,"type":"clients","request_id":"clients-1","current_client_id":"mac-2","clients":[{"client_id":"phone-7","label":"Phone","kinds":["ios"],"connections":1},{"client_id":"mac-2","label":"Mac","kinds":[],"connections":0}]}"#
        guard case .clients(let requestID, let currentClientID, let clients) = try decodeEnvelope(fixture) else {
            return XCTFail("Expected client inventory envelope")
        }
        XCTAssertEqual(requestID, "clients-1")
        XCTAssertEqual(currentClientID, "mac-2")
        XCTAssertEqual(clients.first?.clientId, "phone-7")
        XCTAssertEqual(clients.first?.kinds, [.ios])
        XCTAssertEqual(clients.last?.connections, 0)
    }

    func testSessionCatalogRequestsMatchV8() throws {
        let list = try requestObject(.listSessions(requestID: "list-1"))
        XCTAssertEqual(list["type"] as? String, "list_sessions")
        XCTAssertEqual(list["request_id"] as? String, "list-1")

        let create = try requestObject(.createSession(
            requestID: "create-1",
            workspace: "/srv/horus"
        ))
        XCTAssertEqual(create["type"] as? String, "create_session")
        XCTAssertEqual(create["workspace"] as? String, "/srv/horus")

        let open = try requestObject(.openSession(
            requestID: "open-1",
            sessionID: "chat-1",
            lastSequence: 41
        ))
        XCTAssertEqual(open["type"] as? String, "open_session")
        XCTAssertEqual(open["session_id"] as? String, "chat-1")
        XCTAssertEqual(open["last_sequence"] as? Int, 41)

        let freshOpen = try requestObject(.openSession(
            requestID: "open-2",
            sessionID: "chat-1",
            lastSequence: nil
        ))
        XCTAssertTrue(freshOpen["last_sequence"] is NSNull)
    }

    func testSessionScopedRequestsEncodeSessionID() throws {
        let submission = Submission(id: "input-1", op: .userInput(text: "Hello"))
        let requests: [(GatewayRequest, String)] = [
            (.renameSession(requestID: "rename-1", sessionID: "chat-1", title: "Review"), "rename_session"),
            (.setSessionPinned(requestID: "pin-1", sessionID: "chat-1", pinned: true), "set_session_pinned"),
            (.deleteSession(requestID: "delete-1", sessionID: "chat-1"), "delete_session"),
            (.submit(sessionID: "chat-1", submission: submission), "submit"),
            (.configureSession(
                requestID: "config-1",
                sessionID: "chat-1",
                expectedRevision: 4,
                config: composition
            ), "configure_session"),
            (.getGitDiff(requestID: "diff-1", sessionID: "chat-1"), "get_git_diff"),
            (.switchGitBranch(requestID: "branch-1", sessionID: "chat-1", branch: "feature"), "switch_git_branch"),
            (.listArtifacts(requestID: "artifacts-1", sessionID: "chat-1"), "list_artifacts"),
            (.startCronSetup(requestID: "setup-1", sessionID: "chat-1", task: "Review nightly"), "start_cron_setup"),
            (.listCron(requestID: "cron-1", sessionID: "chat-1"), "list_cron"),
            (.rescheduleCron(requestID: "cron-2", sessionID: "chat-1", id: "task-1", schedule: "0 9 * * *"), "reschedule_cron"),
            (.deleteCron(requestID: "cron-3", sessionID: "chat-1", id: "task-1"), "delete_cron"),
            (.runCron(requestID: "cron-4", sessionID: "chat-1", id: "task-1"), "run_cron"),
            (.listCronHistory(requestID: "cron-5", sessionID: "chat-1", id: nil), "list_cron_history")
        ]

        for (request, type) in requests {
            let object = try requestObject(request)
            XCTAssertEqual(object["type"] as? String, type)
            XCTAssertEqual(object["session_id"] as? String, "chat-1")
        }

        let configure = try requestObject(.configureSession(
            requestID: "config-1",
            sessionID: "chat-1",
            expectedRevision: 4,
            config: composition
        ))
        let encodedConfig = try XCTUnwrap(configure["config"] as? [String: Any])
        let middleware = try XCTUnwrap(encodedConfig["middleware"] as? [String: Any])
        let enabled = try XCTUnwrap(middleware["enabled"] as? [String])
        XCTAssertEqual(Set(enabled), ["skills", "subagents", "tools"])
        let settings = try XCTUnwrap(middleware["settings"] as? [String: Any])
        let subagents = try XCTUnwrap(settings["subagents"] as? [String: Any])
        XCTAssertEqual(subagents["model_route"] as? String, "openai_socket/gpt-5.6-sol")

        let branch = try requestObject(.switchGitBranch(
            requestID: "branch-2",
            sessionID: "chat-1",
            branch: "feature"
        ))
        XCTAssertEqual(branch["branch"] as? String, "feature")

        let cronSetup = try requestObject(.startCronSetup(
            requestID: "setup-2",
            sessionID: "chat-1",
            task: nil
        ))
        XCTAssertTrue(cronSetup["task"] is NSNull)
        let history = try requestObject(.listCronHistory(
            requestID: "cron-6",
            sessionID: "chat-1",
            id: nil
        ))
        XCTAssertTrue(history["id"] is NSNull)
    }

    func testProviderAndUtilityRequestsMatchV8() throws {
        let credential = try requestObject(.setProviderCredential(
            requestID: "credential-1",
            provider: "openai_socket",
            apiKey: "secret"
        ))
        XCTAssertEqual(credential["type"] as? String, "set_provider_credential")
        XCTAssertEqual(credential["api_key"] as? String, "secret")

        let endpointCredential = try requestObject(.setProviderEndpointCredential(
            requestID: "endpoint-1",
            provider: "openai_compatible",
            baseURL: "https://models.example/v1",
            apiKey: "secret"
        ))
        XCTAssertEqual(endpointCredential["base_url"] as? String, "https://models.example/v1")

        let registered = try requestObject(.registerProvider(
            requestID: "register-1",
            config: composition.provider
        ))
        let provider = try XCTUnwrap(registered["config"] as? [String: Any])
        XCTAssertEqual(provider["reasoning_effort"] as? String, "high")
        XCTAssertNil(provider["api_key_env"])

        let requests: [(GatewayRequest, String)] = [
            (.listDirectories(requestID: "directories-1", path: "/srv", includeFiles: true), "list_directories"),
            (.createPairingCode(requestID: "pairing-1"), "create_pairing_code"),
            (.startProviderLogin(requestID: "login-1", provider: "openai_codex"), "start_provider_login"),
            (.getProfile(requestID: "profile-1"), "get_profile")
        ]
        for (request, type) in requests {
            XCTAssertEqual(try requestObject(request)["type"] as? String, type)
        }
    }

    func testConfigureDefaultAgentUsesDefaultRevisionWithoutSessionScope() throws {
        let request = try requestObject(.configureDefaultAgent(
            requestID: "default-1",
            expectedRevision: 4,
            config: composition
        ))

        XCTAssertEqual(request["type"] as? String, "configure_default_agent")
        XCTAssertEqual(request["request_id"] as? String, "default-1")
        XCTAssertEqual(request["expected_revision"] as? Int, 4)
        XCTAssertNil(request["session_id"])
        let config = try XCTUnwrap(request["config"] as? [String: Any])
        let middleware = try XCTUnwrap(config["middleware"] as? [String: Any])
        let settings = try XCTUnwrap(middleware["settings"] as? [String: Any])
        let subagents = try XCTUnwrap(settings["subagents"] as? [String: Any])
        XCTAssertEqual(subagents["model_route"] as? String, "openai_socket/gpt-5.6-sol")

        var inherited = composition
        inherited.middleware.setSetting(nil, middleware: "subagents", setting: "model_route")
        let inheritedRequest = try requestObject(.configureDefaultAgent(
            requestID: "default-2",
            expectedRevision: 4,
            config: inherited
        ))
        let inheritedConfig = try XCTUnwrap(inheritedRequest["config"] as? [String: Any])
        let inheritedMiddleware = try XCTUnwrap(inheritedConfig["middleware"] as? [String: Any])
        let inheritedSettings = try XCTUnwrap(inheritedMiddleware["settings"] as? [String: Any])
        XCTAssertNil(inheritedSettings["subagents"])
    }

    func testGatewayWideReadyPayloadDecodesV8State() throws {
        let envelope = try decodeEnvelope(
            #"{"version":8,"type":"ready","payload":\#(readyPayloadJSON)}"#
        )
        guard case .ready(let payload) = envelope else {
            return XCTFail("Expected ready envelope")
        }

        XCTAssertEqual(payload.sessions.first?.sessionId, "chat-1")
        XCTAssertEqual(payload.sessions.first?.title, "Review")
        XCTAssertEqual(payload.sessions.first?.activity.state, .running)
        XCTAssertEqual(payload.sessions.first?.activity.turnId, "turn-1")
        XCTAssertEqual(payload.providers.first?.defaultApiKeyEnv, "OPENAI_API_KEY")
        XCTAssertEqual(payload.providers.first?.symbol, "sparkle")
        XCTAssertEqual(payload.providers.first?.description, "Persistent Responses API")
        XCTAssertEqual(payload.providers.first?.auth, .apiKey)
        XCTAssertEqual(payload.providers.first?.models.first?.label, "Sol")
        XCTAssertEqual(payload.providers.first?.models.first?.defaultReasoning, "medium")
        XCTAssertEqual(payload.providers.first?.models.first?.reasoning.first?.description, "Balanced reasoning and latency")
        XCTAssertEqual(payload.providers.first?.webSearch, [.off, .cached, .live])
        XCTAssertEqual(payload.defaultConfig?.revision, 4)
        XCTAssertEqual(payload.models.first?.route, "openai_socket/gpt-5.6-sol")
        XCTAssertEqual(payload.middlewareFeatures.first?.id, "skills")
        XCTAssertEqual(payload.maxActiveSessions, 4)
        let settings = try XCTUnwrap(payload.middlewareFeatures.first?.settings)
        guard case .integer(let minimum, let maximum, let step) = settings[0].kind else {
            return XCTFail("Expected integer setting")
        }
        XCTAssertEqual(minimum, 1)
        XCTAssertNil(maximum)
        XCTAssertEqual(step, 10)
        guard case .select(let options, let unsetLabel) = settings[1].kind else {
            return XCTFail("Expected select setting")
        }
        XCTAssertEqual(options.first?.value, "route-a")
        XCTAssertEqual(unsetLabel, "Inherit")

        let configured = try decodeEnvelope(
            #"{"version":8,"type":"gateway_configured","request_id":"gateway-1","payload":\#(readyPayloadJSON)}"#
        )
        guard case .gatewayConfigured(let requestID, let refreshed) = configured else {
            return XCTFail("Expected gateway configured envelope")
        }
        XCTAssertEqual(requestID, "gateway-1")
        XCTAssertEqual(refreshed.maxActiveSessions, 4)
    }

    func testV8RejectsLegacyProviderMetadata() {
        let legacyProvider = #"{"provider":"openai_socket","label":"OpenAI","configured":true,"auth":"api_key","default_model":"gpt-5.6-sol","default_base_url":null,"default_api_key_env":"OPENAI_API_KEY","default_reasoning_effort":"medium","default_web_search":"off"}"#
        let payload = #"{"sessions":[],"providers":[\#(legacyProvider)],"default_config":\#(configJSON),"models":[],"middleware_features":[],"max_active_sessions":4}"#

        XCTAssertThrowsError(try decodeEnvelope(
            #"{"version":8,"type":"ready","payload":\#(payload)}"#
        ))
    }

    func testSessionOpenedAndChangedDecodeSessionReadyPayload() throws {
        let opened = try decodeEnvelope(
            #"{"version":8,"type":"session_opened","request_id":"open-1","payload":\#(sessionReadyPayloadJSON)}"#
        )
        guard case .sessionOpened(let requestID, let payload) = opened else {
            return XCTFail("Expected session opened envelope")
        }
        XCTAssertEqual(requestID, "open-1")
        XCTAssertEqual(payload.latestSequence, 7)
        XCTAssertEqual(payload.workspace.path, "/srv/horus")
        XCTAssertEqual(payload.git?.currentBranch, "main")
        XCTAssertEqual(payload.git?.branches, ["feature", "main"])
        XCTAssertEqual(payload.session.sessionId, "chat-1")
        XCTAssertEqual(payload.toolCount, 7)
        XCTAssertEqual(payload.contributions.first?.count, 2)
        XCTAssertEqual(payload.config.config.middleware.enabled, ["skills", "subagents", "tools"])
        guard let widget = payload.contributions.first?.widgets.first,
              case .picker(let title, let options) = widget.content,
              let option = options.first,
              case .capabilityCommand(let capability, let command, let arguments, let target) =
                  option.op
        else { return XCTFail("Expected widget picker") }
        XCTAssertEqual(title, "Subagents")
        XCTAssertTrue(widget.iconOnly)
        XCTAssertEqual(widget.symbol, "robot")
        XCTAssertEqual(option.description, "running")
        XCTAssertEqual(option.detail, "gpt-5.6-sol")
        XCTAssertEqual(capability, "subagents")
        XCTAssertEqual(command, "subagents")
        XCTAssertEqual(arguments, "reviewer")
        XCTAssertNil(target)

        let changed = try decodeEnvelope(
            #"{"version":8,"type":"session_changed","payload":\#(sessionReadyPayloadJSON)}"#
        )
        guard case .sessionChanged(let changedPayload) = changed else {
            return XCTFail("Expected session changed envelope")
        }
        XCTAssertEqual(changedPayload.workspace.id, "workspace-1")
    }

    func testV8RequiresSessionActivityAndToolCount() {
        let sessionWithoutActivity = #"{"version":8,"type":"sessions","sessions":[{"session_id":"chat-1","session_context":{},"parent_session_id":null,"parent_sequence":null,"sequence":0,"catalog_visible":true,"first_user_message":null,"created_at":100,"updated_at":100,"title":null,"pinned":false}]}"#
        XCTAssertThrowsError(try decodeEnvelope(sessionWithoutActivity))

        let payloadWithoutToolCount = sessionReadyPayloadJSON.replacingOccurrences(
            of: #","tool_count":7"#,
            with: ""
        )
        XCTAssertThrowsError(try decodeEnvelope(
            #"{"version":8,"type":"session_opened","request_id":"open-1","payload":\#(payloadWithoutToolCount)}"#
        ))

        let payloadWithoutContributionCount = sessionReadyPayloadJSON.replacingOccurrences(
            of: #""count":2,"#,
            with: ""
        )
        XCTAssertThrowsError(try decodeEnvelope(
            #"{"version":8,"type":"session_opened","request_id":"open-1","payload":\#(payloadWithoutContributionCount)}"#
        ))
    }

    func testV8RequiresGenericSettingsAndScalarValues() {
        let withoutSettings = configJSON.replacingOccurrences(
            of: #","settings":{"context_offloading":{"stale_after_tokens":50000},"subagents":{"model_route":"openai_socket/gpt-5.6-sol"}}"#,
            with: ""
        )
        let payloadWithoutSettings = readyPayloadJSON.replacingOccurrences(
            of: configJSON,
            with: withoutSettings
        )
        XCTAssertThrowsError(try decodeEnvelope(
            #"{"version":8,"type":"ready","payload":\#(payloadWithoutSettings)}"#
        ))

        let invalidScalar = configJSON.replacingOccurrences(
            of: #""stale_after_tokens":50000"#,
            with: #""stale_after_tokens":true"#
        )
        let payloadWithInvalidScalar = readyPayloadJSON.replacingOccurrences(
            of: configJSON,
            with: invalidScalar
        )
        XCTAssertThrowsError(try decodeEnvelope(
            #"{"version":8,"type":"ready","payload":\#(payloadWithInvalidScalar)}"#
        ))
    }

    func testV8RequiresCacheWriteInputTokens() {
        let usage = #"{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0,"total_tokens":2}"#
        let fixture = #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"token_count","info":{"total_token_usage":\#(usage),"last_token_usage":\#(usage),"model_context_window":200}}},"blocks":[]}"#

        XCTAssertThrowsError(try decodeEnvelope(fixture))
    }

    func testAgentEventFixtureIncludesSessionScope() throws {
        let fixture = #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"submission_id":"input-1","msg":{"type":"session_history","events":[]}},"blocks":[],"history":[{"event":{"type":"frontend","frontend_type":"render","capability":"tools","block":{"id":null,"group":null,"append":false,"pending":false,"text":"Done","format":"plain_text","tone":"neutral"}},"blocks":[]}],"preview":null}"#
        let envelope = try decodeEnvelope(fixture)

        guard case .agentEvent(let sessionID, let sequence, let event, let blocks, let history, let preview) = envelope else {
            return XCTFail("Expected agent event envelope")
        }
        XCTAssertEqual(sessionID, "chat-1")
        XCTAssertEqual(sequence, 8)
        XCTAssertEqual(event.submissionId, "input-1")
        XCTAssertEqual(event.msg["type"]?.stringValue, "session_history")
        XCTAssertTrue(blocks.isEmpty)
        XCTAssertEqual(history?.first?.event["frontendType"]?.stringValue, "render")
        XCTAssertNil(preview)
    }

    func testUnknownAgentEventIsRejected() {
        let fixture = #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"future_event"}},"blocks":[]}"#
        XCTAssertThrowsError(try decodeEnvelope(fixture)) { error in
            XCTAssertEqual(
                error as? GatewayWireError,
                .invalidFrame("unknown agent event future_event")
            )
        }
    }

    func testInvalidFrontendEventSubtypeIsRejected() {
        let fixtures = [
            (#"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"frontend"}},"blocks":[]}"#, "frontend event has no frontend_type"),
            (#"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"frontend","frontend_type":"future_frontend"}},"blocks":[]}"#, "unknown frontend event future_frontend")
        ]

        for (fixture, message) in fixtures {
            XCTAssertThrowsError(try decodeEnvelope(fixture)) { error in
                XCTAssertEqual(error as? GatewayWireError, .invalidFrame(message))
            }
        }
    }

    func testMalformedFrontendEventPayloadIsRejected() {
        let fixtures = [
            (#"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"frontend","frontend_type":"render","capability":"tools"}},"blocks":[]}"#, "frontend render is missing a required field"),
            (#"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"frontend","frontend_type":"picker","title":"Choose","options":[{"label":"One","description":"First"}]}},"blocks":[]}"#, "frontend picker option is missing a required field")
        ]

        for (fixture, message) in fixtures {
            XCTAssertThrowsError(try decodeEnvelope(fixture)) { error in
                XCTAssertEqual(error as? GatewayWireError, .invalidFrame(message))
            }
        }
    }

    func testUnknownRenderedPresentationValuesAreRejected() {
        let outerBlock = #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"task_complete","turn_id":"turn-1","last_agent_message":null}},"blocks":[{"id":null,"group":null,"append":false,"pending":false,"text":"Done","format":"future_format","tone":"neutral"}]}"#
        XCTAssertThrowsError(try decodeEnvelope(outerBlock))

        let invalidWidgetPayload = sessionReadyPayloadJSON.replacingOccurrences(
            of: #""slot":"composer_footer""#,
            with: #""slot":"future_slot""#
        )
        XCTAssertThrowsError(try decodeEnvelope(
            #"{"version":8,"type":"session_opened","request_id":"open-1","payload":\#(invalidWidgetPayload)}"#
        ))
    }

    func testMessageActionWidgetSlotIsAccepted() throws {
        let payload = sessionReadyPayloadJSON.replacingOccurrences(
            of: #""slot":"composer_footer""#,
            with: #""slot":"message_actions""#
        )
        let envelope = try decodeEnvelope(
            #"{"version":8,"type":"session_opened","request_id":"open-1","payload":\#(payload)}"#
        )
        guard case .sessionOpened(_, let ready) = envelope else {
            return XCTFail("Expected session opened envelope")
        }

        XCTAssertEqual(ready.contributions.first?.widgets.first?.slot, "message_actions")
    }

    func testV8MessageTargetsAreStrict() throws {
        let operation = try decoder().decode(
            AgentOperation.self,
            from: Data(
                #"{"type":"capability_command","capability":"sessions","command":"fork","arguments":"","target":{"checkpoint_sequence":12,"batch_item_count":3}}"#.utf8
            )
        )
        guard case .capabilityCommand(_, _, _, let target) = operation else {
            return XCTFail("Expected a capability command")
        }
        XCTAssertEqual(target, MessageTarget(checkpointSequence: 12, batchItemCount: 3))
        let request = try requestObject(.submit(
            sessionID: "chat-1",
            submission: Submission(id: "fork-1", op: operation)
        ))
        let submission = try XCTUnwrap(request["submission"] as? [String: Any])
        let encodedOperation = try XCTUnwrap(submission["op"] as? [String: Any])
        let encodedTarget = try XCTUnwrap(encodedOperation["target"] as? [String: Any])
        XCTAssertEqual(encodedTarget["checkpoint_sequence"] as? Int, 12)
        XCTAssertEqual(encodedTarget["batch_item_count"] as? Int, 3)

        for fixture in [
            #"{"type":"capability_command","capability":"sessions","command":"fork","arguments":""}"#,
            #"{"type":"capability_command","capability":"sessions","command":"fork","arguments":"","target":{"checkpoint_sequence":12,"batch_item_count":0}}"#
        ] {
            XCTAssertThrowsError(
                try decoder().decode(AgentOperation.self, from: Data(fixture.utf8))
            )
        }

        let untargeted = try decodeEnvelope(
            #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"user_message","message":"Hello","message_target":null}},"blocks":[]}"#
        )
        guard case .agentEvent(_, _, let event, _, _, _) = untargeted else {
            return XCTFail("Expected an agent event")
        }
        XCTAssertEqual(event.msg["messageTarget"], .null)
        XCTAssertThrowsError(try decodeEnvelope(
            #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"user_message","message":"Hello"}},"blocks":[]}"#
        ))
    }

    func testMalformedKnownAgentEventIsRejected() {
        let fixtures = [
            #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"turn_aborted","turn_id":"turn-1"}},"blocks":[]}"#,
            #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"agent_message","message":"Working","phase":"future_phase"}},"blocks":[]}"#
        ]

        for fixture in fixtures {
            XCTAssertThrowsError(try decodeEnvelope(fixture))
        }
    }

    func testUnknownAuxiliaryRenderedEventsAreRejected() {
        let fixtures = [
            #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"session_history","events":[]}},"blocks":[],"history":[{"event":{"type":"future_event"},"blocks":[]}]}"#,
            #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"msg":{"type":"frontend","frontend_type":"preview","title":"Worker","events":[]}},"blocks":[],"preview":{"title":"Worker","events":[{"event":{"type":"future_event"},"blocks":[]}]}}"#
        ]

        for fixture in fixtures {
            XCTAssertThrowsError(try decodeEnvelope(fixture)) { error in
                XCTAssertEqual(
                    error as? GatewayWireError,
                    .invalidFrame("unknown agent event future_event")
                )
            }
        }
    }

    func testFrontendRenderAgentEventIsAccepted() throws {
        let fixture = #"{"version":8,"type":"agent_event","session_id":"chat-1","sequence":8,"event":{"submission_id":"input-1","msg":{"type":"frontend","frontend_type":"render","capability":"tools","block":{"id":null,"group":null,"append":false,"pending":false,"text":"Done","format":"plain_text","tone":"neutral"}}},"blocks":[{"id":null,"group":null,"append":false,"pending":false,"text":"Done","format":"plain_text","tone":"neutral"}]}"#
        let envelope = try decodeEnvelope(fixture)

        guard case .agentEvent(_, _, let event, let blocks, _, _) = envelope else {
            return XCTFail("Expected agent event envelope")
        }
        XCTAssertEqual(event.msg["frontendType"]?.stringValue, "render")
        XCTAssertEqual(blocks.first?.text, "Done")
    }

    func testSessionsResponseAllowsOmittedRequestID() throws {
        let envelope = try decodeEnvelope(
            #"{"version":8,"type":"sessions","sessions":[\#(sessionRecordJSON)]}"#
        )
        guard case .sessions(let requestID, let sessions) = envelope else {
            return XCTFail("Expected sessions envelope")
        }
        XCTAssertNil(requestID)
        XCTAssertEqual(sessions.first?.sessionId, "chat-1")
        XCTAssertEqual(sessions.first?.pinned, true)
    }

    func testScopedArtifactGitAndCronResponsesDecodeV8Fields() throws {
        let artifacts = try decodeEnvelope(#"{"version":8,"type":"artifacts","request_id":"artifacts-1","session_id":"chat-1","artifacts":[{"id":"artifact-1","session_id":"chat-1","kind":"code_diff","title":"Patch","block":{"id":null,"group":null,"append":false,"pending":false,"text":"diff","format":"plain_text","tone":"neutral"}}]}"#)
        guard case .artifacts(let artifactRequestID, let artifactSessionID, let records) = artifacts else {
            return XCTFail("Expected artifacts envelope")
        }
        XCTAssertEqual(artifactRequestID, "artifacts-1")
        XCTAssertEqual(artifactSessionID, "chat-1")
        XCTAssertEqual(records.first?.sessionId, "chat-1")
        XCTAssertEqual(records.first?.kind, .codeDiff)

        let gitDiff = try decodeEnvelope(#"{"version":8,"type":"git_diff","request_id":"diff-1","session_id":"chat-1","diff":"diff --git a/a b/a"}"#)
        guard case .gitDiff(let diffRequestID, let diffSessionID, let diff) = gitDiff else {
            return XCTFail("Expected git diff envelope")
        }
        XCTAssertEqual(diffRequestID, "diff-1")
        XCTAssertEqual(diffSessionID, "chat-1")
        XCTAssertTrue(diff.hasPrefix("diff --git"))

        let tasks = try decodeEnvelope(#"{"version":8,"type":"cron_tasks","request_id":"cron-1","session_id":"chat-1","tasks":[{"id":"task-1","session_id":"chat-1","task":"/srv/task.md","schedule":"0 9 * * *"}]}"#)
        guard case .cronTasks(let taskRequestID, let taskSessionID, let cronTasks) = tasks else {
            return XCTFail("Expected cron tasks envelope")
        }
        XCTAssertEqual(taskRequestID, "cron-1")
        XCTAssertEqual(taskSessionID, "chat-1")
        XCTAssertEqual(cronTasks.first?.sessionId, "chat-1")

        let history = try decodeEnvelope(#"{"version":8,"type":"cron_history","request_id":"history-1","session_id":"chat-1","runs":[{"id":"run-1","task_id":"task-1","source_session_id":"chat-1","started_at":100,"finished_at":110,"status":"succeeded","session_id":"chat-2","message":null}]}"#)
        guard case .cronHistory(let historyRequestID, let historySessionID, let runs) = history else {
            return XCTFail("Expected cron history envelope")
        }
        XCTAssertEqual(historyRequestID, "history-1")
        XCTAssertEqual(historySessionID, "chat-1")
        XCTAssertEqual(runs.first?.sourceSessionId, "chat-1")
        XCTAssertEqual(runs.first?.status, .succeeded)
    }

    func testUnknownArtifactKindAndCronRunStatusAreRejected() {
        XCTAssertThrowsError(try decodeEnvelope(
            #"{"version":8,"type":"artifacts","request_id":"artifacts-1","session_id":"chat-1","artifacts":[{"id":"artifact-1","session_id":"chat-1","kind":"future_kind","title":"Patch","block":{"id":null,"group":null,"append":false,"pending":false,"text":"diff","format":"plain_text","tone":"neutral"}}]}"#
        ))
        XCTAssertThrowsError(try decodeEnvelope(
            #"{"version":8,"type":"cron_history","request_id":"history-1","session_id":"chat-1","runs":[{"id":"run-1","task_id":"task-1","source_session_id":"chat-1","started_at":100,"finished_at":110,"status":"future_status","session_id":null,"message":null}]}"#
        ))
    }

    func testControlProviderProfileAndDirectoryResponsesDecode() throws {
        guard case .authenticated = try decodeEnvelope(#"{"version":8,"type":"authenticated"}"#) else {
            return XCTFail("Expected authenticated envelope")
        }
        guard case .accepted(let acceptedID) = try decodeEnvelope(#"{"version":8,"type":"accepted","request_id":"request-1"}"#) else {
            return XCTFail("Expected accepted envelope")
        }
        XCTAssertEqual(acceptedID, "request-1")

        guard case .providerCredentialStatus(let credentialID, let provider, let configured) = try decodeEnvelope(#"{"version":8,"type":"provider_credential_status","request_id":"credential-1","provider":"openai_socket","configured":true}"#) else {
            return XCTFail("Expected provider credential status envelope")
        }
        XCTAssertEqual(credentialID, "credential-1")
        XCTAssertEqual(provider, "openai_socket")
        XCTAssertTrue(configured)

        guard case .pairingCode(let pairingID, let code, let expiresAt) = try decodeEnvelope(#"{"version":8,"type":"pairing_code","request_id":"pairing-1","code":"123456","expires_at":500}"#) else {
            return XCTFail("Expected pairing code envelope")
        }
        XCTAssertEqual(pairingID, "pairing-1")
        XCTAssertEqual(code, "123456")
        XCTAssertEqual(expiresAt, 500)

        guard case .providerLoginStarted(let loginRequestID, let loginID, let loginProvider, let verificationURL, let userCode) = try decodeEnvelope(#"{"version":8,"type":"provider_login_started","request_id":"login-1","login_id":"device-1","provider":"openai_codex","verification_url":"https://example.com/device","user_code":"ABCD"}"#) else {
            return XCTFail("Expected provider login started envelope")
        }
        XCTAssertEqual(loginRequestID, "login-1")
        XCTAssertEqual(loginID, "device-1")
        XCTAssertEqual(loginProvider, "openai_codex")
        XCTAssertEqual(verificationURL, "https://example.com/device")
        XCTAssertEqual(userCode, "ABCD")

        guard case .providerLoginFinished(let finishedRequestID, let finishedLoginID, let finishedProvider) = try decodeEnvelope(#"{"version":8,"type":"provider_login_finished","request_id":"login-1","login_id":"device-1","provider":"openai_codex"}"#) else {
            return XCTFail("Expected provider login finished envelope")
        }
        XCTAssertEqual(finishedRequestID, "login-1")
        XCTAssertEqual(finishedLoginID, "device-1")
        XCTAssertEqual(finishedProvider, "openai_codex")

        guard case .profile(let profileID, let profile) = try decodeEnvelope(#"{"version":8,"type":"profile","request_id":"profile-1","profile":{"user_name":"Ada","daily_usage":[]}}"#) else {
            return XCTFail("Expected profile envelope")
        }
        XCTAssertEqual(profileID, "profile-1")
        XCTAssertEqual(profile.userName, "Ada")

        guard case .directories(let directoryID, let listing) = try decodeEnvelope(#"{"version":8,"type":"directories","request_id":"directories-1","listing":{"path":"/srv","parent":null,"entries":[]}}"#) else {
            return XCTFail("Expected directories envelope")
        }
        XCTAssertEqual(directoryID, "directories-1")
        XCTAssertEqual(listing.path, "/srv")
    }

    func testPairedRejectedAndErrorResponsesDecode() throws {
        guard case .paired(let clientID, let token) = try decodeEnvelope(#"{"version":8,"type":"paired","client_id":"phone-7","token":"bearer"}"#) else {
            return XCTFail("Expected paired envelope")
        }
        XCTAssertEqual(clientID, "phone-7")
        XCTAssertEqual(token, "bearer")

        guard case .rejected(let rejection) = try decodeEnvelope(#"{"version":8,"type":"rejected","request_id":"request-1","code":"conflict","message":"stale","fatal":false}"#) else {
            return XCTFail("Expected rejected envelope")
        }
        XCTAssertEqual(rejection.requestId, "request-1")
        XCTAssertEqual(rejection.code, "conflict")
        XCTAssertFalse(rejection.fatal)

        guard case .error(let failure) = try decodeEnvelope(#"{"version":8,"type":"error","code":"internal","message":"failed","fatal":true}"#) else {
            return XCTFail("Expected error envelope")
        }
        XCTAssertEqual(failure.code, "internal")
        XCTAssertTrue(failure.fatal)
    }

    func testUnknownGatewayMessageAndOperationAreRejected() {
        XCTAssertThrowsError(try decodeEnvelope(#"{"version":8,"type":"future_message"}"#)) { error in
            XCTAssertEqual(
                error as? GatewayWireError,
                .invalidFrame("unknown gateway message future_message")
            )
        }

        let operation = #"{"type":"future_operation"}"#
        XCTAssertThrowsError(try decoder().decode(AgentOperation.self, from: Data(operation.utf8))) { error in
            XCTAssertEqual(
                error as? GatewayWireError,
                .invalidFrame("unknown agent operation future_operation")
            )
        }
    }

    func testUnsupportedGatewayVersionIsRejected() {
        XCTAssertThrowsError(try decodeEnvelope(#"{"version":4,"type":"authenticated"}"#)) { error in
            XCTAssertEqual(error as? GatewayWireError, .unsupportedVersion(4))
        }
    }

    func testPlaintextEndpointIsRestrictedToLoopback() throws {
        XCTAssertEqual(try GatewayEndpoint("tcp://localhost:9191").rawValue, "tcp://localhost:9191")
        XCTAssertThrowsError(try GatewayEndpoint("tcp://example.com:9191")) { error in
            XCTAssertEqual(error as? GatewayWireError, .insecureRemoteEndpoint)
        }
        XCTAssertEqual(try GatewayEndpoint("tls://example.com:443").rawValue, "tls://example.com:443")
    }

    func testSecureWebSocketEndpointUsesImplicitPort443() throws {
        let endpoint = try GatewayEndpoint("wss://gateway.example/")

        XCTAssertEqual(endpoint.rawValue, "wss://gateway.example")
        XCTAssertEqual(endpoint.host, "gateway.example")
        XCTAssertEqual(endpoint.port, 443)
        XCTAssertTrue(endpoint.usesTLS)
        XCTAssertTrue(endpoint.usesWebSocket)
        XCTAssertEqual(
            try GatewayEndpoint("wss://gateway.example:8443").rawValue,
            "wss://gateway.example:8443"
        )
        XCTAssertThrowsError(try GatewayEndpoint("ws://gateway.example"))
        XCTAssertThrowsError(try GatewayEndpoint("wss://gateway.example/chat"))
    }

    func testGatewayAccountsGetFriendlyDefaultNames() throws {
        let quick = GatewayAccount(endpoint: try GatewayEndpoint(
            "wss://pupils-convention-ban-format.trycloudflare.com"
        ))
        let named = GatewayAccount(endpoint: try GatewayEndpoint("wss://gateway.example"))

        XCTAssertEqual(quick.displayName, "Cloudflare · pupils…format")
        XCTAssertEqual(named.displayName, "gateway.example")
    }

    func testPairingSetupParsesValidatedEndpointAndCode() throws {
        let setup = try GatewayPairingSetup(
            "horus-pair:v1|wss://gateway.example:443|0123456789abcdef"
        )

        XCTAssertEqual(setup.endpoint.rawValue, "wss://gateway.example")
        XCTAssertEqual(setup.code, "0123456789abcdef")
    }

    func testPairingSetupRejectsMalformedOrUnsafeValues() {
        let invalid = [
            "horus-pair:v2|wss://gateway.example|code",
            "horus-pair:v1|wss://gateway.example|",
            "horus-pair:v1|wss://gateway.example|code with spaces",
            "horus-pair:v1|wss://gateway.example|café",
            "horus-pair:v1|wss://gateway.example|code|extra",
            "horus-pair:v1|wss://gateway.example|\(String(repeating: "a", count: 513))",
        ]

        for value in invalid {
            XCTAssertThrowsError(try GatewayPairingSetup(value))
        }
        XCTAssertThrowsError(
            try GatewayPairingSetup("horus-pair:v1|tcp://gateway.example:9191|code")
        ) { error in
            XCTAssertEqual(error as? GatewayWireError, .insecureRemoteEndpoint)
        }
    }

    func testPairingURLParsesOnlyTheExactHorusPairRoute() throws {
        let setup = try GatewayPairingSetup(url: try XCTUnwrap(URL(string:
            "horus://pair?endpoint=wss%3A%2F%2Fgateway.example&code=0123456789abcdef"
        )))

        XCTAssertEqual(setup.endpoint.rawValue, "wss://gateway.example")
        XCTAssertEqual(setup.code, "0123456789abcdef")

        let invalid = [
            "other://pair?endpoint=wss%3A%2F%2Fgateway.example&code=code",
            "horus://other?endpoint=wss%3A%2F%2Fgateway.example&code=code",
            "horus://pair/path?endpoint=wss%3A%2F%2Fgateway.example&code=code",
            "horus://pair?endpoint=wss%3A%2F%2Fgateway.example",
            "horus://pair?endpoint=wss%3A%2F%2Fone.example&endpoint=wss%3A%2F%2Ftwo.example&code=code",
            "horus://pair?endpoint=wss%3A%2F%2Fgateway.example&code=code&extra=value",
            "horus://pair?endpoint=wss%3A%2F%2Fgateway.example&code=code#fragment",
        ]
        for value in invalid {
            XCTAssertThrowsError(
                try GatewayPairingSetup(url: try XCTUnwrap(URL(string: value)))
            )
        }
    }

    func testSameRevisionRefreshPreservesProviderDraft() {
        let snapshot = VersionedAgentConfig(revision: 4, config: composition)
        var draft = composition
        draft.provider.provider = "openrouter"

        let refreshed = refreshedAgentDraft(
            currentDraft: draft,
            currentSnapshot: snapshot,
            incomingSnapshot: snapshot
        )

        XCTAssertEqual(refreshed.provider.provider, "openrouter")
    }
}
