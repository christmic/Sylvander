import SwiftUI

@main
struct Token9App: App {
    var body: some Scene {
        MenuBarExtra {
            DashboardView()
                .preferredColorScheme(.dark)
        } label: {
            // The seed-crab mark loads as PDF first (sharp at Retina),
            // falls back to the wrapped PNG when only the raster is
            // present. See commit 1 (assets: add token9 seed crab mark).
            Image("SeedCrabMark", bundle: .module)
        }
        .menuBarExtraStyle(.window)
    }
}