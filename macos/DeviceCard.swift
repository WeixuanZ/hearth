import AppKit
import Charts
import SwiftUI

/// A device: a summary that opens, in place, into controls made from its
/// capabilities. Nothing here knows a brand, so a new device needs no UI.
struct DeviceCard: View {
    @ObservedObject var model: BridgeModel
    let device: Device
    @State private var open: Bool
    /// The name being typed, while renaming.
    @State private var draft: String?
    @FocusState private var naming: Bool

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
        .contextMenu {
            Button("Rename…", action: beginRename)
        }
    }

    private func beginRename() {
        draft = device.name
        // Once the field exists.
        DispatchQueue.main.async { naming = true }
    }

    private func endRename(save: Bool) {
        if save, let draft { model.rename(device, draft) }
        draft = nil
    }

    @ViewBuilder
    private var header: some View {
        if draft != nil {
            HStack(spacing: 8) {
                Image(systemName: symbol)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.secondary)
                    .frame(width: 16)
                TextField("Name", text: Binding(get: { draft ?? "" }, set: { draft = $0 }))
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 13, weight: .semibold))
                    .focused($naming)
                    .onSubmit { endRename(save: true) }
                    .onExitCommand { endRename(save: false) }
                    .onChange(of: naming) { focused in
                        if !focused { endRename(save: true) }
                    }
                Button("Done") { endRename(save: true) }
                    .controlSize(.small)
            }
        } else {
            summaryHeader
        }
    }

    private var summaryHeader: some View {
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
            if open {
                Button(action: beginRename) {
                    Image(systemName: "pencil")
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Rename")
            }
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
        let readings = Array(all("reading").filter(\.active).prefix(3))
        if !readings.isEmpty {
            Readings(device: device, readings: readings, grid: false)
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
                Readings(device: device, readings: readings, grid: true)
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
    /// The key last pressed on the keyboard, lit for a moment.
    @State private var lit: String?

    /// This pad's place in `KeyboardRemote`.
    private var owner: String { "\(device.id)/\(keys.id)" }

    var body: some View {
        let all = keys.keys ?? []
        let has = Set(all.map(\.id))
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(keys.name).font(.system(size: 12))
                Spacer(minLength: 8)
                Label("Use your keyboard", systemImage: "keyboard")
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
            }
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
        .onAppear { KeyboardRemote.shared.claim(owner, press: typed) }
        .onDisappear { KeyboardRemote.shared.release(owner) }
    }

    /// Presses a key typed on the keyboard, if the pad has it and can use it
    /// now. Reads the device afresh, since this outlives the view's copy.
    private func typed(_ id: String) -> Bool {
        guard let device = model.status?.devices.first(where: { $0.id == device.id }),
              let keys = device.capabilities.first(where: { $0.id == keys.id }),
              device.online, keys.active, keys.keys?.contains(where: { $0.id == id }) == true
        else { return false }
        model.set(device, keys, .text(id))
        lit = id
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
            if lit == id { lit = nil }
        }
        return true
    }

    private var blank: some View {
        Color.clear.frame(width: Self.size.width, height: Self.size.height)
    }

    private func key(_ id: String) -> some View {
        let name = keys.keys?.first { $0.id == id }?.name ?? id
        return Button {
            model.set(device, keys, .text(id))
            KeyboardRemote.shared.claim(owner, press: typed)
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
        .buttonStyle(KeyStyle(lit: lit == id))
        .help(name)
    }
}

/// A remote key: a quiet rounded square that darkens while pressed, by
/// mouse or keyboard.
private struct KeyStyle: ButtonStyle {
    var lit = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(.primary)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color.primary.opacity(configuration.isPressed || lit ? 0.18 : 0.07))
            )
            .contentShape(Rectangle())
    }
}

/// Sends keys typed in the panel to the open remote pad last opened or used.
/// Typing in a text field, and shortcuts with Command, Control or Option,
/// pass through untouched.
private final class KeyboardRemote {
    static let shared = KeyboardRemote()

    /// Open pads, the most recent last. Each presses a key id, or says no.
    private var pads: [(owner: String, press: (String) -> Bool)] = []
    private var monitor: Any?

    func claim(_ owner: String, press: @escaping (String) -> Bool) {
        pads.removeAll { $0.owner == owner }
        pads.append((owner, press))
        if monitor == nil {
            monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
                self?.handle(event) == true ? nil : event
            }
        }
    }

    func release(_ owner: String) {
        pads.removeAll { $0.owner == owner }
        if pads.isEmpty, let monitor {
            NSEvent.removeMonitor(monitor)
            self.monitor = nil
        }
    }

    private func handle(_ event: NSEvent) -> Bool {
        guard let pad = pads.last,
              event.modifierFlags.isDisjoint(with: [.command, .control, .option]),
              !(event.window?.firstResponder is NSText),
              let id = Self.key(event)
        else { return false }
        // Escape still cancels first where something cancels on it.
        if id == "back", event.keyCode == 53, event.window?.performKeyEquivalent(with: event) == true {
            return true
        }
        return pad.press(id)
    }

    /// The remote key a keyboard key stands for.
    private static func key(_ event: NSEvent) -> String? {
        switch event.specialKey {
        case .upArrow: return "up"
        case .downArrow: return "down"
        case .leftArrow: return "left"
        case .rightArrow: return "right"
        case .carriageReturn, .enter: return "select"
        case .delete: return "back"
        default: break
        }
        switch event.charactersIgnoringModifiers?.lowercased() {
        case "\u{1b}": return "back"
        case " ": return "play"
        case "h": return "home"
        case "i": return "info"
        case "m": return "mute"
        case "+", "=": return "volume_up"
        case "-", "_": return "volume_down"
        default: return nil
        }
    }
}

private struct Readings: View {
    let device: Device
    let readings: [Capability]
    /// Three in a row, or a wrapping grid when the card is open.
    var grid: Bool
    @State private var hovered: String?
    @State private var cache: [String: HistorySeries] = [:]
    @State private var fetched: [String: Date] = [:]
    @State private var loading: String?
    @State private var failure: String?
    @State private var interval = 60
    @State private var heartbeat = 900

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if grid {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 76), alignment: .leading)],
                    alignment: .leading,
                    spacing: 8
                ) {
                    ForEach(readings) { reading in
                        cell(reading)
                    }
                }
            } else {
                HStack(alignment: .top, spacing: 16) {
                    ForEach(readings) { reading in
                        cell(reading)
                    }
                }
            }
            if let hovered, let reading = readings.first(where: { $0.id == hovered }) {
                ReadingHistory(
                    reading: reading,
                    series: cache[hovered],
                    interval: interval,
                    heartbeat: heartbeat,
                    loading: loading == hovered && cache[hovered] == nil,
                    failure: cache[hovered] == nil ? failure : nil
                )
            }
        }
        .onHover { inside in
            // Cleared here, not on the number, so the pointer can move onto the chart.
            if !inside { hovered = nil }
        }
    }

    private func cell(_ reading: Capability) -> some View {
        ReadingView(reading: reading)
            .padding(.vertical, 1)
            .contentShape(Rectangle())
            .onHover { inside in
                if inside {
                    hovered = reading.id
                    load(reading.id)
                }
            }
    }

    /// The last day is enough to glance at, and small enough to ask for on hover.
    private func load(_ id: String) {
        if let at = fetched[id], cache[id] != nil, Date().timeIntervalSince(at) < 60 {
            return
        }
        if loading == id { return }
        loading = id
        failure = nil
        let deviceId = device.id
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                let data = try ControlSocket.request([
                    "op": "history",
                    "device": deviceId,
                    "capability": id,
                    "hours": 24,
                ])
                let reply = try JSONDecoder().decode(HistoryReply.self, from: data)
                let series = reply.series.first ?? HistorySeries(
                    capability: id, name: id, unit: "", samples: [])
                DispatchQueue.main.async {
                    cache[id] = series
                    fetched[id] = Date()
                    interval = reply.interval
                    heartbeat = reply.heartbeat
                    if loading == id { loading = nil }
                }
            } catch {
                let message = (error as? ControlSocket.Failure)?.message ?? "Couldn't load history"
                DispatchQueue.main.async {
                    failure = message
                    if loading == id { loading = nil }
                }
            }
        }
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

private struct PlotPoint: Identifiable {
    let id: Int
    let time: Date
    let value: Double
    /// Breaks the line across a gap, so an hour switched off is not a slope.
    let segment: Int
}

private struct ReadingHistory: View {
    let reading: Capability
    let series: HistorySeries?
    let interval: Int
    let heartbeat: Int
    let loading: Bool
    let failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let series, !series.samples.isEmpty {
                chart(series)
                    .frame(height: 88)
            }
            Text(caption)
                .font(.system(size: 10))
                .foregroundStyle(.tertiary)
                .lineLimit(2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func chart(_ series: HistorySeries) -> some View {
        let points = plots(series)
        let values = series.samples.map(\.v)
        let low = values.min() ?? 0
        let high = values.max() ?? 0
        let pad = max((high - low) * 0.2, 0.5)
        return Chart(points) { point in
            LineMark(
                x: .value("Time", point.time),
                y: .value(reading.name, point.value),
                series: .value("Run", point.segment)
            )
            .interpolationMethod(.linear)
            .foregroundStyle(Color.primary.opacity(0.85))
            if points.count < 2 {
                PointMark(
                    x: .value("Time", point.time),
                    y: .value(reading.name, point.value)
                )
                .foregroundStyle(Color.primary.opacity(0.85))
            }
        }
        .chartYScale(domain: (low - pad)...(high + pad))
        .chartXAxis {
            AxisMarks(values: .automatic(desiredCount: 3)) { value in
                AxisGridLine()
                AxisValueLabel {
                    if let date = value.as(Date.self) {
                        Text(date, format: .dateTime.hour())
                            .font(.system(size: 9))
                            .foregroundStyle(.tertiary)
                    }
                }
            }
        }
        .chartYAxis {
            AxisMarks(position: .leading, values: .automatic(desiredCount: 3)) { value in
                AxisGridLine()
                AxisValueLabel {
                    if let number = value.as(Double.self) {
                        Text(format(number, nil))
                            .font(.system(size: 9))
                            .foregroundStyle(.tertiary)
                    }
                }
            }
        }
        .chartLegend(.hidden)
    }

    private func plots(_ series: HistorySeries) -> [PlotPoint] {
        var points: [PlotPoint] = []
        var segment = 0
        var previous: Double?
        let breakAfter = Double(heartbeat + interval)
        for (index, sample) in series.samples.enumerated() {
            if let previous, sample.t - previous > breakAfter {
                segment += 1
            }
            previous = sample.t
            points.append(PlotPoint(
                id: index,
                time: Date(timeIntervalSince1970: sample.t),
                value: sample.v,
                segment: segment
            ))
        }
        return points
    }

    private var caption: String {
        if let failure, series == nil {
            return failure
        }
        guard let series else {
            return loading ? "Loading the last 24 hours…" : "No readings in the last 24 hours"
        }
        guard let low = series.samples.map(\.v).min(), let high = series.samples.map(\.v).max() else {
            return "No readings in the last 24 hours"
        }
        if abs(high - low) < 0.0005 {
            return "\(format(low, reading.unit)) · 24 hours"
        }
        return "\(format(low, reading.unit))–\(format(high, reading.unit)) · 24 hours"
    }
}

/// A number with its unit: whole numbers without a point.
private func format(_ number: Double, _ unit: String?) -> String {
    let text = number.rounded() == number ? String(Int(number)) : String(format: "%.1f", number)
    guard let unit, !unit.isEmpty else { return text }
    return unit == "%" ? "\(text)%" : "\(text) \(unit)"
}
