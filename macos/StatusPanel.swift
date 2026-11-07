import AppKit
import SwiftUI

/// The flame in the menu bar, and the panel it opens.
///
/// SwiftUI's `MenuBarExtra` sizes its window by its own rules, and newer
/// macOS made that window larger than the panel, leaving the panel floating
/// in a clear box. Here the window is always exactly the panel's size, and
/// grows and shrinks with it, its top kept under the menu bar.
@MainActor
final class StatusPanel: NSObject {
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let window: PanelWindow
    private let hosting: FittingHostingView<AnyView>
    private var outsideClicks: Any?
    private var resigned: NSObjectProtocol?

    /// Between the menu bar and the panel.
    private static let gap: CGFloat = 5

    init<Content: View>(@ViewBuilder content: () -> Content) {
        hosting = FittingHostingView(rootView: AnyView(content().fixedSize()))
        window = PanelWindow(
            contentRect: NSRect(origin: .zero, size: hosting.fittingSize),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: true
        )
        super.init()

        window.isOpaque = false
        window.backgroundColor = .clear
        // Glass draws its own edge and shadow; the window's would be its
        // square outline, a dark box around the rounded panel.
        window.hasShadow = !Self.glass
        window.level = .statusBar
        window.collectionBehavior = [.moveToActiveSpace, .fullScreenAuxiliary]
        window.hidesOnDeactivate = false
        window.isReleasedWhenClosed = false
        window.animationBehavior = .utilityWindow

        // Only its natural size: the window, not constraints, follows it.
        hosting.sizingOptions = [.intrinsicContentSize]
        window.contentView = Self.background(around: hosting)
        hosting.onResize = { [weak self] size in self?.resize(to: size) }

        if let button = item.button {
            button.image = NSImage(systemSymbolName: "flame", accessibilityDescription: "Hearth")
            button.target = self
            button.action = #selector(toggle)
        }
    }

    var shown: Bool { window.isVisible }

    @objc private func toggle() {
        shown ? close() : open()
    }

    func open() {
        resize(to: hosting.fittingSize)
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
        // After the click that opened it has been handled, which unhighlights.
        Task { @MainActor [weak self] in self?.item.button?.highlight(true) }
        // A click anywhere else closes the panel, as it would a menu.
        guard outsideClicks == nil else { return }
        outsideClicks = NSEvent.addGlobalMonitorForEvents(
            matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown]
        ) { [weak self] _ in Task { @MainActor in self?.close() } }
        resigned = NotificationCenter.default.addObserver(
            forName: NSApplication.didResignActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in Task { @MainActor in self?.close() } }
    }

    func close() {
        if let outsideClicks { NSEvent.removeMonitor(outsideClicks) }
        if let resigned { NotificationCenter.default.removeObserver(resigned) }
        outsideClicks = nil
        resigned = nil
        window.orderOut(nil)
        item.button?.highlight(false)
    }

    /// The panel's size, its top under the flame, kept on the screen.
    private func resize(to size: NSSize) {
        guard size.width > 0, size.height > 0 else { return }
        guard let button = item.button, let buttonWindow = button.window else {
            window.setContentSize(size)
            return
        }
        let anchor = buttonWindow.convertToScreen(button.convert(button.bounds, to: nil))
        let screen = buttonWindow.screen ?? NSScreen.main
        var x = (anchor.midX - size.width / 2).rounded()
        if let visible = screen?.visibleFrame {
            x = min(max(x, visible.minX + 8), visible.maxX - size.width - 8)
        }
        let top = anchor.minY - Self.gap
        let frame = NSRect(x: x, y: top - size.height, width: size.width, height: size.height)
        guard frame != window.frame else { return }
        window.setFrame(frame, display: true)
        if window.hasShadow { window.invalidateShadow() }
    }

    /// Whether the panel is Liquid Glass, from macOS 26.
    private static var glass: Bool {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) { return true }
        #endif
        return false
    }

    /// What macOS puts behind its own menu bar windows: Liquid Glass from
    /// macOS 26, the menu's material before.
    private static func background(around content: NSView) -> NSView {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            let glass = NSGlassEffectView()
            glass.cornerRadius = 16
            glass.contentView = content
            return glass
        }
        #endif
        let effect = NSVisualEffectView()
        effect.material = .menu
        effect.blendingMode = .behindWindow
        effect.state = .active
        effect.maskImage = roundedMask(radius: 10)
        content.frame = effect.bounds
        content.autoresizingMask = [.width, .height]
        effect.addSubview(content)
        return effect
    }

    /// A stretchable rounded rectangle, so the corners stay round at any size.
    private static func roundedMask(radius: CGFloat) -> NSImage {
        let edge = radius * 2 + 1
        let image = NSImage(size: NSSize(width: edge, height: edge), flipped: false) { rect in
            NSColor.black.setFill()
            NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
            return true
        }
        image.capInsets = NSEdgeInsets(top: radius, left: radius, bottom: radius, right: radius)
        image.resizingMode = .stretch
        return image
    }
}

/// Takes the keyboard, for the text fields in the panel, without a title bar.
private final class PanelWindow: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

/// Says when the SwiftUI content's own size changes, as cards open, close,
/// or devices arrive.
private final class FittingHostingView<Content: View>: NSHostingView<Content> {
    var onResize: ((NSSize) -> Void)?

    override func invalidateIntrinsicContentSize() {
        super.invalidateIntrinsicContentSize()
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.onResize?(self.fittingSize)
        }
    }
}
