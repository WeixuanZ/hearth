import SwiftUI

/// A machine that runs programmes, like a dishwasher: its settings, then
/// Start. Running, the time left is the centre of the card, the settings
/// fold into one line, the controls for a running programme appear, and
/// Stop takes Start's place.
///
/// The device's order says which is which: controls before the run set it
/// up, controls after it are for while it runs.
struct RunSection: View {
    @ObservedObject var model: BridgeModel
    let device: Device
    let run: Capability

    private var running: Bool { run.value.bool }

    private var settings: [Capability] { controls(before: true) }
    private var whileRunning: [Capability] { controls(before: false) }

    private func controls(before: Bool) -> [Capability] {
        let at = device.capabilities.firstIndex { $0.id == run.id } ?? device.capabilities.endIndex
        let side = before ? device.capabilities[..<at] : device.capabilities[at...]
        return side.filter { ["switch", "choice", "level"].contains($0.kind) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if running {
                RunDial(run: run, fixed: fixedSummary)
                    .frame(maxWidth: .infinity)
                    .transition(.opacity.combined(with: .scale(scale: 0.96)))
            }
            ForEach(running ? whileRunning : settings) { capability in
                ControlRow(model: model, device: device, capability: capability)
                    .disabled(!capability.active || !device.online)
                    .opacity(capability.active ? 1 : 0.45)
                    .transition(.opacity)
            }
            RunButton(model: model, device: device, run: run)
                .padding(.top, 2)
        }
        .animation(.easeInOut(duration: 0.25), value: running)
    }

    /// What the running programme was set up with, like "Eco 50 · Extra dry".
    private var fixedSummary: String {
        settings.compactMap { capability in
            switch capability.kind {
            case "choice":
                return capability.options?.first { $0.id == capability.value.text }?.name
            case "switch":
                return capability.value.bool ? capability.name : nil
            default:
                return nil
            }
        }
        .joined(separator: " · ")
    }
}

/// Time left, in a ring that fills as the programme goes.
private struct RunDial: View {
    let run: Capability
    let fixed: String

    var body: some View {
        VStack(spacing: 8) {
            ZStack {
                Circle()
                    .stroke(Color.primary.opacity(0.08), lineWidth: 7)
                Circle()
                    .trim(from: 0, to: (run.progress ?? 0) / 100)
                    .stroke(
                        Color.accentColor.gradient,
                        style: StrokeStyle(lineWidth: 7, lineCap: .round)
                    )
                    .rotationEffect(.degrees(-90))
                    .animation(.easeInOut(duration: 0.6), value: run.progress)
                VStack(spacing: 0) {
                    Text(left.figure)
                        .font(.system(size: 30, weight: .semibold, design: .rounded))
                        .monospacedDigit()
                        .contentTransition(.numericText())
                    Text(left.unit)
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
            }
            .frame(width: 118, height: 118)
            .accessibilityElement(children: .combine)
            if !fixed.isEmpty {
                Text(fixed)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 4)
    }

    /// "3:40 hours left" over an hour, "42 minutes left" under.
    private var left: (figure: String, unit: String) {
        guard let seconds = run.remaining else { return ("–", "running") }
        let minutes = Int((seconds / 60).rounded(.up))
        if minutes >= 60 {
            return (String(format: "%d:%02d", minutes / 60, minutes % 60), "hours left")
        }
        return ("\(minutes)", minutes == 1 ? "minute left" : "minutes left")
    }
}

/// Start, or Stop. Either waits three seconds, filling as it counts, so a
/// press can be taken back with another.
private struct RunButton: View {
    @ObservedObject var model: BridgeModel
    let device: Device
    let run: Capability

    private static let wait = 3

    @State private var counting: Int?
    @State private var filled = false
    @State private var timer: Timer?
    /// Sent, until the machine says it has started or stopped.
    @State private var sentFor: Bool?

    private var running: Bool { run.value.bool }
    private var usable: Bool { run.active && device.online }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Group {
                if let counting {
                    countdown(counting)
                } else if sentFor == !running {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text(running ? "Stopping…" : "Starting…")
                    }
                    .frame(maxWidth: .infinity, minHeight: 30)
                } else {
                    Button(action: begin) {
                        Label(running ? "Stop" : "Start", systemImage: running ? "stop.fill" : "play.fill")
                    }
                    .buttonStyle(CapsuleButton(tint: running ? .red : .accentColor))
                    .disabled(!usable)
                }
            }
            .animation(.easeInOut(duration: 0.2), value: counting == nil)
            if let blocked = run.blocked, !running || !usable {
                Label(blocked, systemImage: "info.circle")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .onChange(of: running) { _ in sentFor = nil }
        .onDisappear(perform: cancel)
    }

    private func countdown(_ left: Int) -> some View {
        let tint: Color = running ? .red : .accentColor
        return Button(action: cancel) {
            ZStack(alignment: .leading) {
                GeometryReader { geometry in
                    Capsule()
                        .fill(tint.opacity(0.85))
                        .frame(width: filled ? geometry.size.width : 0)
                }
                HStack {
                    Text(running ? "Stopping in \(left)…" : "Starting in \(left)…")
                        .monospacedDigit()
                        .contentTransition(.numericText())
                    Spacer()
                    Text("Cancel").fontWeight(.semibold)
                }
                .font(.system(size: 13, weight: .medium))
                .padding(.horizontal, 12)
            }
            .frame(height: 30)
            .background(Capsule().fill(tint.opacity(0.18)))
            .clipShape(Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help("Cancel")
    }

    private func begin() {
        counting = Self.wait
        filled = false
        withAnimation(.linear(duration: Double(Self.wait))) { filled = true }
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in
            guard let left = counting else { return }
            if left > 1 {
                withAnimation { counting = left - 1 }
            } else {
                timer?.invalidate()
                counting = nil
                sentFor = !running
                model.set(device, run, .bool(!running))
                // A refusal shows as the model's problem; the button comes back.
                DispatchQueue.main.asyncAfter(deadline: .now() + 10) { sentFor = nil }
            }
        }
    }

    private func cancel() {
        timer?.invalidate()
        timer = nil
        withAnimation { counting = nil }
        filled = false
    }
}

/// A full-width capsule in the tint, the same in a window that is not key,
/// where macOS greys a prominent button.
private struct CapsuleButton: ButtonStyle {
    let tint: Color
    @Environment(\.isEnabled) private var enabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(enabled ? Color.white : Color.secondary)
            .frame(maxWidth: .infinity, minHeight: 30)
            .background(
                Capsule().fill(enabled ? AnyShapeStyle(tint.gradient) : AnyShapeStyle(Color.primary.opacity(0.08)))
            )
            .opacity(configuration.isPressed ? 0.8 : 1)
            .contentShape(Capsule())
    }
}

/// While a programme runs, a closed card shows how far it has to go.
struct RunSummary: View {
    let run: Capability

    var body: some View {
        if run.value.bool {
            HStack(spacing: 8) {
                ProgressView(value: (run.progress ?? 0) / 100)
                    .progressViewStyle(.linear)
                    .tint(.accentColor)
                if let seconds = run.remaining {
                    Text(remaining(seconds))
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                        .fixedSize()
                }
            }
        }
    }

    private func remaining(_ seconds: Double) -> String {
        let minutes = Int((seconds / 60).rounded(.up))
        return minutes >= 60
            ? "\(minutes / 60) h \(minutes % 60) min left"
            : "\(minutes) min left"
    }
}
