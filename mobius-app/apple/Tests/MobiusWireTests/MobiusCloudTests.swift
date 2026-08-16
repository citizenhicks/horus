import CryptoKit
import Foundation
import Security
import XCTest

@MainActor
final class MobiusCloudTests: XCTestCase {
    func testAppleNonceUsesThirtyTwoRandomBytesAndSHA256() throws {
        let nonce = try MobiusCloudAppleNonce.make()
        var encoded = nonce.rawValue
            .replacing("-", with: "+")
            .replacing("_", with: "/")
        encoded += String(repeating: "=", count: (4 - encoded.count % 4) % 4)
        let rawBytes = try XCTUnwrap(Data(base64Encoded: encoded))
        let expectedHash = SHA256.hash(data: Data(nonce.rawValue.utf8)).map { byte in
            let hex = String(byte, radix: 16)
            return byte < 16 ? "0\(hex)" : hex
        }.joined()

        XCTAssertEqual(rawBytes.count, 32)
        XCTAssertEqual(nonce.rawValue.count, 43)
        XCTAssertEqual(nonce.requestValue, expectedHash)
    }

    func testClientUsesNativeCloudContractAndBearerFromDeviceOnlyKeychain() async throws {
        let userID = UUID()
        let token = String(repeating: "t", count: 43)
        let service = "app.mobius.cloud.tests.\(UUID())"
        let store = MobiusCloudSessionStore(service: service)
        defer { try? store.remove() }
        var requests: [URLRequest] = []
        let responses = [
            #"{"token":"\#(token)","userId":"\#(userID.uuidString)","expiresAt":"2099-01-01T00:00:00Z"}"#,
            #"{"accepted":true}"#,
            #"{"status":"ready"}"#,
            #"{"endpoint":"wss://gateway.example","pairingCode":"0123456789abcdef","expiresAt":"2099-01-01T00:00:00Z"}"#,
        ]
        let client = MobiusCloudClient(store: store) { request in
            requests.append(request)
            let index = requests.count - 1
            return try self.response(for: request, json: responses[index])
        }

        let session = try await client.authenticate(
            authorizationCode: "apple-code",
            nonce: String(repeating: "n", count: 43)
        )
        try await client.submitSubscription(signedTransaction: "header.payload.signature")
        let status = try await client.gatewayStatus()
        let grant = try await client.createPairingGrant()

        XCTAssertEqual(session.userID, userID)
        XCTAssertEqual(status, .ready)
        XCTAssertEqual(grant.setup.endpoint.rawValue, "wss://gateway.example")
        XCTAssertEqual(grant.setup.code, "0123456789abcdef")
        XCTAssertEqual(requests.map { $0.url?.path }, [
            "/api/mobile/auth/apple",
            "/api/mobile/subscription",
            "/api/mobile/gateway",
            "/api/mobile/gateway",
        ])
        XCTAssertEqual(requests.map(\.httpMethod), ["POST", "PUT", "GET", "POST"])
        XCTAssertNil(requests[0].value(forHTTPHeaderField: "Authorization"))
        for request in requests.dropFirst() {
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer \(token)")
        }
        let authBody = try XCTUnwrap(requests[0].httpBody)
        let authJSON = try XCTUnwrap(
            JSONSerialization.jsonObject(with: authBody) as? [String: String]
        )
        XCTAssertEqual(authJSON, [
            "authorizationCode": "apple-code",
            "nonce": String(repeating: "n", count: 43),
        ])

        let attributes = try keychainAttributes(service: service)
        XCTAssertEqual(
            attributes[kSecAttrAccessible as String] as? String,
            kSecAttrAccessibleWhenUnlockedThisDeviceOnly as String
        )
    }

    func testCloudPairingGrantRejectsUnsafeGatewayFields() async throws {
        let store = MobiusCloudSessionStore(service: "app.mobius.cloud.tests.\(UUID())")
        defer { try? store.remove() }
        var requestCount = 0
        let client = MobiusCloudClient(store: store) { request in
            requestCount += 1
            let json = requestCount == 1
                ? #"{"token":"ttttttttttttttttttttttttttttttttttttttttttt","userId":"00000000-0000-0000-0000-000000000001","expiresAt":"2099-01-01T00:00:00Z"}"#
                : #"{"endpoint":"tcp://gateway.example:8741","pairingCode":"code","expiresAt":"2099-01-01T00:00:00Z"}"#
            return try self.response(for: request, json: json)
        }
        _ = try await client.authenticate(
            authorizationCode: "apple-code",
            nonce: String(repeating: "n", count: 43)
        )

        do {
            _ = try await client.createPairingGrant()
            XCTFail("Expected unsafe pairing fields to be rejected")
        } catch {
            XCTAssertTrue(error is MobiusCloudError)
        }
    }

    private func response(
        for request: URLRequest,
        status: Int = 200,
        json: String
    ) throws -> (Data, HTTPURLResponse) {
        let url = try XCTUnwrap(request.url)
        let response = try XCTUnwrap(HTTPURLResponse(
            url: url,
            statusCode: status,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"]
        ))
        return (Data(json.utf8), response)
    }

    private func keychainAttributes(service: String) throws -> [String: Any] {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: "mobile-session",
            kSecReturnAttributes: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        XCTAssertEqual(status, errSecSuccess)
        return try XCTUnwrap(result as? [String: Any])
    }
}
