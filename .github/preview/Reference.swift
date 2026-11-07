// A stock SwiftUI MenuBarExtra, opened on its own, to compare with Hearth's panel.
import AppKit
import SwiftUI

@main
struct Reference: App {
    @NSApplicationDelegateAdaptor(Opener.self) private var opener
    var body: some Scene {
        MenuBarExtra("Reference", systemImage: "star") {
            VStack(alignment: .leading, spacing: 12) {
                Text("Reference MenuBarExtra").font(.system(size: 15, weight: .semibold))
                Text("A stock SwiftUI window-style menu bar extra, for comparison.")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
                HStack { Button("Start") {}.buttonStyle(.bordered); Spacer(); Button("Quit") {} }
            }
            .padding(16)
            .frame(width: 312, alignment: .leading)
        }
        .menuBarExtraStyle(.window)
    }
}

final class Opener: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            for window in NSApp.windows where String(describing: type(of: window)).contains("StatusBar") {
                if let button = Self.button(in: window.contentView) { button.performClick(nil) }
            }
        }
    }
    static func button(in view: NSView?) -> NSStatusBarButton? {
        guard let view else { return nil }
        if let button = view as? NSStatusBarButton { return button }
        for sub in view.subviews { if let found = button(in: sub) { return found } }
        return nil
    }
}
