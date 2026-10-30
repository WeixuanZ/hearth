import AppKit
import SwiftUI

struct Panel: View {
    @ObservedObject var model: BridgeModel
    @ObservedObject var updater: Updater

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if let group = model.status?.group, model.running, GroupCard.shows(group) {
                GroupCard(model: model, group: group)
                    .padding(.top, 14)
            }
            // Home's pairing is changed on the Mac that serves it.
            if model.forgetting || (model.running && model.status?.group?.serving != false) {
                HomeCard(model: model)
                    .padding(.top, 14)
            }
            if let devices = model.status?.devices, model.running, !devices.isEmpty {
                VStack(spacing: 8) {
                    ForEach(devices) { device in
                        DeviceCard(model: model, device: device)
                    }
                    if let problem = model.problem {
                        Label(problem, systemImage: "exclamationmark.triangle")
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .padding(.top, 14)
            } else {
                empty
                    .padding(.top, 16)
            }
            footer
                .padding(.top, 16)
        }
        .padding(16)
        .frame(width: 312, alignment: .leading)
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 8) {
            Text(model.status?.name ?? "Hearth")
                .font(.system(size: 15, weight: .semibold))
            Spacer(minLength: 8)
            HStack(spacing: 6) {
                Circle()
                    .fill(model.running
                        ? Color(red: 0.18, green: 0.70, blue: 0.44)
                        : Color.primary.opacity(0.22))
                    .frame(width: 7, height: 7)
                Text(state)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var state: String {
        guard model.running else { return "Stopped" }
        switch model.status?.group?.role {
        case "standby": return "Standby"
        case "joining": return "Joining"
        case "looking": return "Starting"
        default: return "Serving Home"
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
                update
                Spacer()
                Text("v\(updater.version)")
                    .font(.system(size: 11))
                    .foregroundStyle(.tertiary)
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

extension Panel {
    @ViewBuilder
    fileprivate var update: some View {
        switch updater.state {
        case .available(let tag):
            Button("Update to \(tag)") { updater.install(tag, model: model) }
                .buttonStyle(.borderedProminent)
                .controlSize(.regular)
        case .installing:
            ProgressView().controlSize(.small)
            Text("Updating")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
        case .failed(let message):
            Image(systemName: "exclamationmark.triangle")
                .foregroundStyle(.secondary)
                .help(message)
        case .current:
            EmptyView()
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
                Text(model.status?.pin ?? "")
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
            Text("In Home: Add Accessory, More options, then \(model.status?.name ?? "Hearth"). Enter this code.")
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

