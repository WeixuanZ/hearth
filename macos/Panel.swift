import AppKit
import SwiftUI

struct Panel: View {
    @ObservedObject var model: BridgeModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if let devices = model.snapshot?.devices, model.running, !devices.isEmpty {
                VStack(spacing: 8) {
                    ForEach(devices) { device in
                        DeviceCard(device: device)
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
            Text(model.snapshot?.name ?? "Hearth")
                .font(.system(size: 15, weight: .semibold))
            Spacer(minLength: 8)
            HStack(spacing: 6) {
                Circle()
                    .fill(model.running
                        ? Color(red: 0.18, green: 0.70, blue: 0.44)
                        : Color.primary.opacity(0.22))
                    .frame(width: 7, height: 7)
                Text(model.running ? "Running" : "Stopped")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.secondary)
            }
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
            if model.running, let pin = model.snapshot?.pin, !pin.isEmpty {
                HStack(spacing: 8) {
                    Text("Pairing")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 8)
                    Text(pin)
                        .font(.system(size: 12, weight: .medium, design: .monospaced))
                    Button {
                        model.copyPin()
                    } label: {
                        Image(systemName: model.copied ? "checkmark" : "doc.on.doc")
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.borderless)
                    .help("Copy the pairing code")
                }
            }
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

private struct DeviceCard: View {
    let device: DeviceSnap

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
