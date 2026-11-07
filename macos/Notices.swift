import AppKit
import SwiftUI

/// A card at the top of the panel: an icon, what happened, what it means,
/// and what to do about it.
struct NoticeCard<Actions: View>: View {
    let symbol: String
    let tint: Color
    let title: String
    let message: String
    @ViewBuilder var actions: Actions

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 18, weight: .medium))
                .foregroundStyle(tint)
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                Text(message)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    actions
                }
                .controlSize(.small)
                .padding(.top, 6)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(tint.opacity(0.10))
        )
    }
}

/// macOS keeps the app off the local network until it is allowed, and asks
/// again after an update. The bridge says when the network refuses it.
struct NetworkCard: View {
    private static let settings = URL(
        string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_LocalNetwork"
    )!

    var body: some View {
        NoticeCard(
            symbol: "wifi.exclamationmark",
            tint: .orange,
            title: "Allow Hearth on your network",
            message: "macOS is keeping Hearth off the local network, so it can't reach your devices. They're all kept, and come back once Hearth is allowed. macOS asks again after an update."
        ) {
            Button("Open Settings") { NSWorkspace.shared.open(Self.settings) }
                .buttonStyle(.borderedProminent)
        }
    }
}

/// A newer Hearth: what it is, and installing it.
struct UpdateCard: View {
    @ObservedObject var updater: Updater
    @ObservedObject var model: BridgeModel

    var body: some View {
        switch updater.state {
        case .available(let tag):
            NoticeCard(
                symbol: "arrow.down.circle.fill",
                tint: .accentColor,
                title: "Hearth \(Updater.plain(tag)) is available",
                message: "You have \(updater.version). Your devices, Home and settings stay as they are; Hearth restarts on its own."
            ) {
                Button("Update") { updater.install(tag, model: model) }
                    .buttonStyle(.borderedProminent)
                Button("What's New") { NSWorkspace.shared.open(Updater.notes(tag)) }
                    .buttonStyle(.bordered)
                Button("Later") { updater.skip(tag) }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.secondary)
            }
        case .installing(let tag):
            NoticeCard(
                symbol: "arrow.down.circle",
                tint: .accentColor,
                title: "Updating to \(Updater.plain(tag))",
                message: "Downloading and checking it. Hearth restarts when it's ready."
            ) {
                ProgressView().progressViewStyle(.linear).frame(maxWidth: .infinity)
            }
        case .failed(let message):
            NoticeCard(
                symbol: "exclamationmark.triangle.fill",
                tint: .orange,
                title: "The update didn't install",
                message: message
            ) {
                Button("Try Again") { updater.retry(model: model) }
                    .buttonStyle(.bordered)
                Button("Dismiss") { updater.state = .current }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.secondary)
            }
        case .latest:
            NoticeCard(
                symbol: "checkmark.circle.fill",
                tint: .green,
                title: "Hearth is up to date",
                message: "\(updater.version) is the latest version."
            ) {
                EmptyView()
            }
        case .current:
            EmptyView()
        }
    }

    static func shows(_ state: Updater.State) -> Bool { state != .current }
}
