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
    }
}
