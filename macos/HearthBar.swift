import AppKit
import SwiftUI

@main
struct HearthBar: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var app

    // The menu bar's panel is AppKit's (see StatusPanel); SwiftUI needs a
    // scene all the same.
    var body: some Scene {
        Settings { EmptyView() }
    }
}

/// Holds what lives as long as the app, and takes the addresses sign-in
/// pages send the browser back to (see `CFBundleURLTypes` in Info.plist).
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = BridgeModel()
    let updater = Updater()
    let setup = SetupSession()
    let launch = Launch()
    private var panel: StatusPanel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        panel = StatusPanel { [model, updater, setup, launch] in
            Panel(model: model, updater: updater, setup: setup, launch: launch)
        }
        // PREVIEW-ONLY: opens the panel for CI screenshots. Removed before merge.
        if ProcessInfo.processInfo.environment["HEARTH_SHOW_PANEL"] != nil {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in self?.panel?.open() }
        }
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        urls.forEach(setup.receive)
        panel?.open()
    }
}
