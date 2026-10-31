import AppKit
import SwiftUI

@main
struct HearthBar: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var app

    var body: some Scene {
        MenuBarExtra {
            Panel(model: app.model, updater: app.updater, setup: app.setup)
        } label: {
            Image(systemName: "flame")
        }
        .menuBarExtraStyle(.window)
    }
}

/// Holds what lives as long as the app, and takes the addresses sign-in
/// pages send the browser back to (see `CFBundleURLTypes` in Info.plist).
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = BridgeModel()
    let updater = Updater()
    let setup = SetupSession()

    func application(_ application: NSApplication, open urls: [URL]) {
        urls.forEach(setup.receive)
    }
}
