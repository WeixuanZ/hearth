import AppKit
import SwiftUI

struct Panel: View {
    @ObservedObject var model: BridgeModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if model.adding {
                AddDevice(model: model)
                    .padding(.top, 14)
            } else if model.running || model.forgetting {
                HomeCard(model: model)
                    .padding(.top, 14)
            }
            if !model.adding, let devices = model.snapshot?.devices, model.running, !devices.isEmpty {
                VStack(spacing: 8) {
                    ForEach(devices) { device in
                        DeviceCard(device: device, model: model)
                    }
                }
                .padding(.top, 14)
            } else if !model.adding {
                empty
                    .padding(.top, 16)
            }
            footer
                .padding(.top, 16)
        }
        .padding(16)
        .frame(width: 320, alignment: .leading)
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 8) {
            Text(model.snapshot?.name ?? "Hearth")
                .font(.system(size: 15, weight: .semibold))
            Spacer(minLength: 8)
            HStack(spacing: 6) {
                Circle()
                    .fill(model.running
                        ? Color(red: 0.18, green: 0.70, blue: 0.44)
                        : Color.primary.opacity(0.22))
                    .frame(width: 7, height: 7)
                Text(statusLine)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var statusLine: String {
        switch model.snapshot?.role {
        case "standby":
            let who = model.snapshot?.leader ?? ""
            return who.isEmpty ? "Following another Mac" : "Following \(who)"
        case "conflict":
            return "Two different Homes"
        case "starting":
            return "Looking for other Macs"
        default:
            return model.running ? "Serving Home" : "Stopped"
        }
    }

    private var empty: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(model.running ? "No devices yet" : "The bridge is not running")
                .font(.system(size: 13, weight: .medium))
            Text(model.missingBinary
                ? "Build it with cargo install, then open this again."
                : "Start it here, or leave the launch agent in charge.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let notice = model.notice, !notice.isEmpty {
                Text(notice)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let tag = model.updateTag {
                Button(model.updating ? "Updating \(tag)" : "Update to \(tag)") {
                    model.installUpdate()
                }
                .buttonStyle(.bordered)
                .disabled(model.updating)
            }
            Toggle("Start at login", isOn: Binding(
                get: { model.loginItem },
                set: { model.setLogin($0) }
            ))
            .font(.system(size: 12))
            .toggleStyle(.switch)
            .controlSize(.small)
            HStack(spacing: 8) {
                Button(model.starting ? "Starting" : (model.running ? "Stop" : "Start")) {
                    if model.running {
                        model.stop()
                    } else {
                        model.start()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.regular)
                .disabled(model.starting)
                Button("Add") { model.adding = true }
                    .buttonStyle(.bordered)
                    .disabled(model.products.isEmpty)
                Spacer()
                Button("Quit") {
                    NSApplication.shared.terminate(nil)
                }
                .buttonStyle(.borderless)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .keyboardShortcut("q")
            }
        }
    }
}

/// Where the bridge stands with Apple Home: ready to add, added, or being
/// forgotten. The code shows only when it is needed, to add the bridge.
private struct HomeCard: View {
    @ObservedObject var model: BridgeModel
    @State private var confirming = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if model.forgetting {
                row(symbol: "house", title: "Forgetting Home…") {
                    ProgressView().controlSize(.small)
                }
            } else if confirming {
                confirm
            } else if model.paired == true {
                row(symbol: "house.fill", title: "In Apple Home") {
                    Menu {
                        Button("Forget Home…") { confirming = true }
                    } label: {
                        Image(systemName: "ellipsis.circle")
                    }
                    .menuStyle(.borderlessButton)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .help("More")
                }
            } else {
                add
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.primary.opacity(0.045))
        )
    }

    private var add: some View {
        VStack(alignment: .leading, spacing: 10) {
            row(symbol: "house", title: "Add to Apple Home") { EmptyView() }
            HStack(spacing: 8) {
                Text(model.snapshot?.pin ?? "")
                    .font(.system(size: 20, weight: .semibold, design: .monospaced))
                    .textSelection(.enabled)
                Button {
                    model.copyPin()
                } label: {
                    Image(systemName: model.copied ? "checkmark" : "doc.on.doc")
                }
                .buttonStyle(.borderless)
                .help("Copy the code")
            }
            .padding(.leading, 24)
            Text("In Home: Add Accessory, More options, then \(model.snapshot?.name ?? "Hearth"). Enter this code.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.leading, 24)
            Button("Home says it is already added?") { confirming = true }
                .buttonStyle(.link)
                .font(.system(size: 12))
                .padding(.leading, 24)
        }
    }

    private var confirm: some View {
        VStack(alignment: .leading, spacing: 10) {
            row(symbol: "exclamationmark.triangle", title: "Forget Apple Home?") { EmptyView() }
            Text("For a bridge Home will not add again. It restarts as a new accessory. If Home still lists the old one, remove it there.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.leading, 24)
            HStack(spacing: 8) {
                Spacer()
                Button("Cancel") { confirming = false }
                    .keyboardShortcut(.cancelAction)
                Button("Forget", role: .destructive) {
                    confirming = false
                    model.forgetHome()
                }
                .buttonStyle(.borderedProminent)
                .tint(.red)
            }
        }
    }

    private func row<Trailing: View>(
        symbol: String,
        title: String,
        @ViewBuilder trailing: () -> Trailing
    ) -> some View {
        HStack(alignment: .center, spacing: 8) {
            Image(systemName: symbol)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(.secondary)
                .frame(width: 16)
            Text(title)
                .font(.system(size: 13, weight: .semibold))
            Spacer(minLength: 8)
            trailing()
        }
    }
}

private struct DeviceCard: View {
    let device: DeviceSnap
    @ObservedObject var model: BridgeModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: symbol)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.secondary)
                    .frame(width: 16)
                Text(device.name)
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1)
                Spacer(minLength: 8)
                Text(device.headline)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(device.online ? Color.primary : Color.secondary)
                Menu {
                    Button("Remove from Home", role: .destructive) {
                        model.removeDevice(device.name)
                    }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .frame(width: 16)
            }
            if !device.detail.isEmpty {
                Text(device.detail)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .padding(.leading, 24)
            }
            if !device.metrics.isEmpty {
                HStack(alignment: .top, spacing: 16) {
                    ForEach(Array(device.metrics.prefix(4))) { metric in
                        VStack(alignment: .leading, spacing: 1) {
                            Text(metric.value)
                                .font(.system(size: 12, weight: .medium, design: .rounded))
                            Text(metric.label)
                                .font(.system(size: 10))
                                .foregroundStyle(.tertiary)
                        }
                    }
                }
                .padding(.leading, 24)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.primary.opacity(0.045))
        )
    }

    private var symbol: String {
        switch device.kind {
        case "dyson":
            return "fanblades"
        case "frame":
            return "tv"
        default:
            return "circle.dashed"
        }
    }
}
