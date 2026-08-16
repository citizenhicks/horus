import SwiftUI

@main
struct MobiusAppleApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            AppShell()
                .environment(model)
                .mobiusTheme()
                .onOpenURL { model.handleOpenURL($0) }
        }
    }
}
