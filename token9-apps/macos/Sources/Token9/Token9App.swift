import SwiftUI

@main
struct Token9App: App {
    var body: some Scene {
        MenuBarExtra("token9", systemImage: "timer") {
            DashboardView()
                .preferredColorScheme(.dark)
        }
        .menuBarExtraStyle(.window)
    }
}
