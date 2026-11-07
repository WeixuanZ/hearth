import ServiceManagement
import SwiftUI

/// Whether Hearth opens with the Mac, and starts the bridge when it opens.
/// Together they keep Home in reach after a restart, with nobody at the Mac.
///
/// Opening at login is macOS's own login item, which the person can also
/// turn off in System Settings; the rest is remembered here.
@MainActor
final class Launch: ObservableObject {
    nonisolated private static let decidedKey = "launch.decided"
    nonisolated private static let startsBridgeKey = "launch.startsBridge"

    /// Read before the panel exists, as the app opens.
    nonisolated static var startsBridge: Bool {
        UserDefaults.standard.bool(forKey: startsBridgeKey)
    }

    @Published private(set) var atLogin = false
    /// macOS asks the person to allow it in System Settings first.
    @Published private(set) var needsApproval = false
    @Published private(set) var decided: Bool
    @Published var startsBridge: Bool {
        didSet { UserDefaults.standard.set(startsBridge, forKey: Self.startsBridgeKey) }
    }
    @Published private(set) var problem: String?

    private let service = SMAppService.mainApp

    init() {
        decided = UserDefaults.standard.bool(forKey: Self.decidedKey)
        startsBridge = Self.startsBridge
        refresh()
    }

    /// macOS's view, which System Settings may have changed.
    func refresh() {
        atLogin = service.status == .enabled
        needsApproval = service.status == .requiresApproval
    }

    func setAtLogin(_ on: Bool) {
        do {
            if on {
                try service.register()
            } else {
                try service.unregister()
            }
            problem = nil
        } catch {
            problem = "macOS would not change the login item: \(error.localizedDescription)"
        }
        refresh()
    }

    /// The answer to the first question: keep Hearth running, or not now.
    func choose(keepRunning: Bool) {
        if keepRunning {
            setAtLogin(true)
            startsBridge = true
        }
        decided = true
        UserDefaults.standard.set(true, forKey: Self.decidedKey)
    }

    func openLoginItems() {
        SMAppService.openSystemSettingsLoginItems()
    }
}

/// Asked once: whether Hearth should start with the Mac.
struct LaunchCard: View {
    @ObservedObject var launch: Launch

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "power.circle")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(.secondary)
                    .frame(width: 16)
                Text("Start Hearth with your Mac?")
                    .font(.system(size: 13, weight: .semibold))
            }
            Text("Home reaches your devices only while the bridge runs. Hearth can open when you log in and start it, so a restart doesn't take them out of Home.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Button("Start at Login") { launch.choose(keepRunning: true) }
                    .buttonStyle(.borderedProminent)
                Button("Not Now") { launch.choose(keepRunning: false) }
                    .buttonStyle(.bordered)
                Spacer()
            }
            .controlSize(.small)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.primary.opacity(0.045))
        )
    }
}

/// The footer's gear: how Hearth runs, updates, and quitting.
struct SettingsMenu: View {
    @ObservedObject var launch: Launch
    @ObservedObject var updater: Updater

    var body: some View {
        Menu {
            Toggle("Open at Login", isOn: Binding(get: { launch.atLogin }, set: launch.setAtLogin))
            Toggle("Start the Bridge When Hearth Opens", isOn: $launch.startsBridge)
            if launch.needsApproval {
                Button("Allow in Login Items…", action: launch.openLoginItems)
            }
            if let problem = launch.problem {
                Text(problem)
            }
            Divider()
            Button("Check for Updates…") { updater.check(asked: true) }
            Text("Hearth \(updater.version)")
            Divider()
            Button("Quit Hearth") { NSApplication.shared.terminate(nil) }
                .keyboardShortcut("q")
        } label: {
            Image(systemName: "gearshape")
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .frame(width: 26, height: 22)
                .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Settings")
        .onAppear(perform: launch.refresh)
    }
}

/// A footer button: an icon and a word, in the secondary colour, with a
/// soft highlight under the pointer, like the system's own panels.
struct FooterButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        Highlighted(pressed: configuration.isPressed) {
            configuration.label
                .labelStyle(.titleAndIcon)
                .font(.system(size: 12, weight: .medium))
        }
    }

    private struct Highlighted<Content: View>: View {
        let pressed: Bool
        @ViewBuilder var content: Content
        @State private var hovering = false
        @Environment(\.isEnabled) private var enabled

        var body: some View {
            content
                .foregroundStyle(hovering && enabled ? .primary : .secondary)
                .padding(.horizontal, 8)
                .padding(.vertical, 5)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Color.primary.opacity(pressed ? 0.14 : (hovering && enabled ? 0.08 : 0)))
                )
                .contentShape(Rectangle())
                .onHover { hovering = $0 }
                .animation(.easeOut(duration: 0.12), value: hovering)
        }
    }
}
