import Foundation
import XCTest

@MainActor
extension AppModelTests {
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

}
