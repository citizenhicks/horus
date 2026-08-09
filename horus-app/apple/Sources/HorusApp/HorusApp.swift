import SwiftUI

@main
struct HorusAppleApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            AppShell()
                .environment(model)
                .horusTheme()
                .onOpenURL { model.handleOpenURL($0) }
        }
#if os(macOS)
        .defaultSize(width: 1180, height: 780)
        .windowToolbarStyle(.unified)
#endif
    }
}
