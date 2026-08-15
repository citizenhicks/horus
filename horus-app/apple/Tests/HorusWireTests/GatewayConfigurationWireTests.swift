import Foundation
import XCTest

extension GatewayWireTests {
    func testProviderAndUtilityRequestsMatchV28() throws {
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
            config: composition.provider,
            modelIds: ["gpt-5.6-sol", "gpt-5.6-mini"],
            reasoningEfforts: ["medium", "high"]
        ))
        let provider = try XCTUnwrap(registered["config"] as? [String: Any])
        XCTAssertEqual(provider["reasoning_effort"] as? String, "high")
        XCTAssertNil(provider["api_key_env"])
        XCTAssertEqual(registered["model_ids"] as? [String], ["gpt-5.6-sol", "gpt-5.6-mini"])
        XCTAssertEqual(registered["reasoning_efforts"] as? [String], ["medium", "high"])

        let directory = try requestObject(.createWorkspaceDirectory(
            requestID: "create-directory-1",
            parent: "/srv",
            name: "New Project"
        ))
        XCTAssertEqual(directory["type"] as? String, "create_workspace_directory")
        XCTAssertEqual(directory["parent"] as? String, "/srv")
        XCTAssertEqual(directory["name"] as? String, "New Project")

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
        XCTAssertEqual(config["max_model_steps"] as? Int, 256)
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
