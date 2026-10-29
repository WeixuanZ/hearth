import SwiftUI

/// A device: a summary that opens, in place, into controls made from its
/// capabilities. Nothing here knows a brand, so a new device needs no UI.
struct DeviceCard: View {
    @ObservedObject var model: BridgeModel
    let device: Device
    @State private var open: Bool

    init(model: BridgeModel, device: Device, open: Bool = false) {
        self.model = model
        self.device = device
        _open = State(initialValue: open)
    }

    private func all(_ kind: String) -> [Capability] {
        device.capabilities.filter { $0.kind == kind }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if open {
                controls
            } else {
                summary
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.primary.opacity(0.045))
        )
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 8) {
            Button {
                withAnimation(.easeInOut(duration: 0.18)) { open.toggle() }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: symbol)
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 16)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(device.name)
                            .font(.system(size: 13, weight: .semibold))
                            .lineLimit(2)
                            .fixedSize(horizontal: false, vertical: true)
                        Text(device.headline)
                            .font(.system(size: 11))
                            .foregroundStyle(device.online ? .secondary : .tertiary)
                    }
                    Spacer(minLength: 8)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if let power = all("power").first {
                Toggle("Power", isOn: model.binding(device, power))
                    .toggleStyle(.switch)
                    .controlSize(.mini)
                    .labelsHidden()
                    .disabled(!device.online)
            }
        }
    }

    @ViewBuilder
    private var summary: some View {
        let readings = all("reading").filter(\.active).prefix(3)
        if !readings.isEmpty {
            HStack(alignment: .top, spacing: 16) {
                ForEach(Array(readings)) { reading in
                    ReadingView(reading: reading)
                }
            }
            .padding(.leading, 24)
        }
    }

    private var controls: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(device.capabilities.filter { ["switch", "choice", "level"].contains($0.kind) }) {
                capability in
                ControlRow(model: model, device: device, capability: capability)
                    .disabled(!capability.active || !device.online)
                    .opacity(capability.active ? 1 : 0.45)
            }
            ForEach(all("keys")) { keys in
                RemotePad(model: model, device: device, keys: keys)
                    .disabled(!keys.active || !device.online)
                    .opacity(keys.active ? 1 : 0.45)
            }
            let readings = all("reading")
            if !readings.isEmpty {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 76), alignment: .leading)],
                    alignment: .leading,
                    spacing: 8
                ) {
                    ForEach(readings) { reading in
                        ReadingView(reading: reading)
                    }
                }
                .padding(.top, 2)
            }
        }
        .padding(.leading, 24)
    }

    private var symbol: String {
        switch device.category {
        case "purifier": return "fanblades"
        case "tv": return "tv"
        default: return "circle.dashed"
        }
    }
}

extension BridgeModel {
    /// A toggle's binding to a power or switch capability.
    func binding(_ device: Device, _ capability: Capability) -> Binding<Bool> {
        Binding(
            get: { capability.value.bool },
            set: { self.set(device, capability, .bool($0)) }
        )
    }
}

/// One switch, choice or level.
private struct ControlRow: View {
    @ObservedObject var model: BridgeModel
    let device: Device
    let capability: Capability
    /// A slider's position while it is dragged. Sent when let go.
    @State private var draft: Double?

    var body: some View {
        switch capability.kind {
        case "switch":
            HStack {
                Text(capability.name).font(.system(size: 12))
                Spacer(minLength: 8)
                Toggle(capability.name, isOn: model.binding(device, capability))
                    .toggleStyle(.switch)
                    .controlSize(.mini)
                    .labelsHidden()
            }
        case "choice":
            let options = capability.options ?? []
            let picker = Picker(capability.name, selection: selection) {
                ForEach(options) { option in
                    Text(option.name).tag(option.id)
                }
            }
            .labelsHidden()
            .controlSize(.small)
            // Segments only when they fit the card, like a sweep's four
            // angles. More, like a TV's inputs, become a menu.
            if options.count <= 4 && options.map(\.name.count).reduce(0, +) <= 20 {
                VStack(alignment: .leading, spacing: 4) {
                    Text(capability.name).font(.system(size: 12))
                    picker.pickerStyle(.segmented).fixedSize()
                }
            } else {
                HStack {
                    Text(capability.name).font(.system(size: 12))
                    Spacer(minLength: 8)
                    picker.pickerStyle(.menu).fixedSize()
                }
            }
        case "level":
            let low = capability.min ?? 0
            let high = capability.max ?? 100
            let value = draft ?? capability.value.number ?? low
            VStack(alignment: .leading, spacing: 2) {
                HStack {
                    Text(capability.name).font(.system(size: 12))
                    Spacer()
                    Text(format(value, capability.unit))
                        .font(.system(size: 12, weight: .medium, design: .rounded))
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                }
                // Snapped by hand: a stepped Slider draws a tick per step.
                let step = capability.step ?? 1
                Slider(
                    value: Binding(get: { value }, set: { draft = ($0 / step).rounded() * step }),
                    in: low...high
                ) { editing in
                    if !editing, let sent = draft {
                        model.set(device, capability, .number(sent))
                        draft = nil
                    }
                }
                .controlSize(.small)
            }
        default:
            EmptyView()
        }
    }

    private var selection: Binding<String> {
        Binding(
            get: { capability.value.text ?? "" },
            set: { model.set(device, capability, .text($0)) }
        )
    }
}

/// A remote: arrows around Select, then whatever other keys the device has.
private struct RemotePad: View {
    @ObservedObject var model: BridgeModel
    let device: Device
    let keys: Capability

    private static let symbols: [String: String] = [
        "up": "chevron.up", "down": "chevron.down", "left": "chevron.left",
        "right": "chevron.right", "select": "circle.fill", "back": "arrow.uturn.backward",
        "home": "house", "play": "playpause", "info": "info.circle", "menu": "gearshape",
        "volume_up": "speaker.plus", "volume_down": "speaker.minus", "mute": "speaker.slash",
    ]
    private static let arrows = ["up", "down", "left", "right", "select"]
    /// One key's size. Arrows (3 wide) and the rest (4 wide) fit the card.
    private static let size = CGSize(width: 26, height: 24)
    private static let gap: CGFloat = 3

    var body: some View {
        let all = keys.keys ?? []
        let has = Set(all.map(\.id))
        VStack(alignment: .leading, spacing: 6) {
            Text(keys.name).font(.system(size: 12))
            HStack(alignment: .center, spacing: 12) {
                if Self.arrows.allSatisfy(has.contains) {
                    Grid(horizontalSpacing: Self.gap, verticalSpacing: Self.gap) {
                        GridRow { blank; key("up"); blank }
                        GridRow { key("left"); key("select"); key("right") }
                        GridRow { blank; key("down"); blank }
                    }
                }
                LazyVGrid(
                    columns: Array(
                        repeating: GridItem(.fixed(Self.size.width), spacing: Self.gap), count: 4),
                    spacing: Self.gap
                ) {
                    ForEach(all.filter { !Self.arrows.contains($0.id) }) { option in
                        key(option.id)
                    }
                }
                .fixedSize()
            }
        }
    }

    private var blank: some View {
        Color.clear.frame(width: Self.size.width, height: Self.size.height)
    }

    private func key(_ id: String) -> some View {
        let name = keys.keys?.first { $0.id == id }?.name ?? id
        return Button {
            model.set(device, keys, .text(id))
        } label: {
            Group {
                if let symbol = Self.symbols[id] {
                    Image(systemName: symbol)
                        .font(.system(size: id == "select" ? 6 : 11, weight: .semibold))
                } else {
                    Text(name).font(.system(size: 10))
                }
            }
            .frame(width: Self.size.width, height: Self.size.height)
        }
        .buttonStyle(KeyStyle())
        .help(name)
    }
}

/// A remote key: a quiet rounded square that darkens while pressed.
private struct KeyStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(.primary)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color.primary.opacity(configuration.isPressed ? 0.18 : 0.07))
            )
            .contentShape(Rectangle())
    }
}

private struct ReadingView: View {
    let reading: Capability

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(reading.value.number.map { format($0, reading.unit) } ?? "–")
                .font(.system(size: 12, weight: .medium, design: .rounded))
                .monospacedDigit()
            Text(reading.name)
                .font(.system(size: 10))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
        }
        .opacity(reading.active ? 1 : 0.45)
    }
}

/// A number with its unit: whole numbers without a point.
private func format(_ number: Double, _ unit: String?) -> String {
    let text = number.rounded() == number ? String(Int(number)) : String(format: "%.1f", number)
    guard let unit, !unit.isEmpty else { return text }
    return unit == "%" ? "\(text)%" : "\(text) \(unit)"
}
