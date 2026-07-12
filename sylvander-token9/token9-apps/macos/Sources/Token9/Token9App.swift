import AppKit
import SwiftUI

/// Optional visible-window launcher for development and support.
/// Normal launches remain menu-bar-only; pass `--window` to open the
/// dashboard immediately when the menu-bar item is hidden by macOS.
final class Token9AppDelegate: NSObject, NSApplicationDelegate {
    private var dashboardWindow: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard ProcessInfo.processInfo.arguments.contains("--window") else { return }

        let controller = NSHostingController(
            rootView: DashboardView().preferredColorScheme(.dark)
        )
        let window = NSWindow(contentViewController: controller)
        window.title = "token9"
        window.styleMask = [.titled, .closable, .miniaturizable]
        window.setContentSize(NSSize(width: L.popoverW, height: L.popoverH))
        window.center()
        window.isReleasedWhenClosed = false
        window.makeKeyAndOrderFront(nil)
        dashboardWindow = window
        NSApp.activate(ignoringOtherApps: true)
    }
}

@main
struct Token9App: App {
    @NSApplicationDelegateAdaptor(Token9AppDelegate.self) private var appDelegate

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
