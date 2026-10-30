import AppKit
import Charts
import SwiftUI

/// How far back a reading's history goes.
enum HistorySpan: Int, CaseIterable, Identifiable {
    case day = 24
    case week = 168

    var id: Int { rawValue }
    var label: String { self == .day ? "24 hours" : "7 days" }
}

/// A reading's history, in the manner of Apple Weather's detail cards: the
/// value large, the day's high and low, and either the last day as a line
/// coloured by value, which the pointer scrubs, or the last week as ranges.
struct HistoryCard: View {
    let device: String
    let reading: Capability
    @State private var span = HistorySpan.day
    @State private var replies: [HistorySpan: HistoryReply] = [:]
    @State private var failure: String?
    /// The moment under the pointer, while scrubbing the day.
    @State private var scrub: HistorySample?
    var now = Date()

    init(
        device: String, reading: Capability, span: HistorySpan = .day,
        preset: [HistorySpan: HistoryReply] = [:], now: Date = Date()
    ) {
        self.device = device
        self.reading = reading
        self.now = now
        _span = State(initialValue: span)
        _replies = State(initialValue: preset)
    }

    var body: some View {
        let reply = replies[span]
        let samples = reply?.series.first?.samples ?? []
        VStack(alignment: .leading, spacing: 12) {
            header(samples)
            Picker("Span", selection: $span) {
                ForEach(HistorySpan.allCases) { Text($0.label).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            if let reply, !samples.isEmpty {
                switch span {
                case .day:
                    DayChart(
                        reading: reading, samples: samples,
                        gap: Double(reply.heartbeat + reply.interval), now: now, scrub: $scrub)
                        .frame(height: 150)
                case .week:
                    WeekBars(reading: reading, samples: samples, now: now)
                }
            } else {
                Text(failure ?? (reply == nil ? "Loading…" : "Nothing measured in the last \(span.label)"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 150)
            }
        }
        .padding(16)
        .frame(width: 340)
        .task(id: span) { await load() }
    }

    private func header(_ samples: [HistorySample]) -> some View {
        let shown = scrub ?? samples.last
        let values = samples.map(\.v)
        return VStack(alignment: .leading, spacing: 2) {
            Text(reading.name.uppercased())
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.secondary)
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                HStack(alignment: .firstTextBaseline, spacing: 3) {
                    Text(shown.map { label($0.v) } ?? reading.value.number.map(label) ?? "–")
                        .font(.system(size: 30, weight: .semibold, design: .rounded))
                        .monospacedDigit()
                    if compactUnit(reading.unit) == nil, let unit = reading.unit {
                        Text(unit)
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(.secondary)
                    }
                }
                if let low = values.min(), let high = values.max() {
                    Text("H \(label(high))  L \(label(low))")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                }
            }
            Text(scrub.map { Date(timeIntervalSince1970: $0.t).formatted(date: .omitted, time: .shortened) } ?? "Now")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
        }
    }

    private func label(_ value: Double) -> String {
        format(value, compactUnit(reading.unit))
    }

    private func load() async {
        if replies[span] != nil { return }
        let request: [String: Any] = ["op": "history", "device": device, "capability": reading.id, "hours": span.rawValue]
        do {
            let data = try await Task.detached { try ControlSocket.request(request) }.value
            replies[span] = try JSONDecoder().decode(HistoryReply.self, from: data)
            failure = nil
        } catch {
            failure = (error as? ControlSocket.Failure)?.message ?? "Couldn't load the history"
        }
    }
}

/// The last day: a smooth line coloured by value over a soft fill, broken
/// where the reading was off. Now is ringed; the pointer scrubs.
private struct DayChart: View {
    let reading: Capability
    let samples: [HistorySample]
    /// Seconds between two samples beyond which the line breaks.
    let gap: Double
    let now: Date
    @Binding var scrub: HistorySample?

    private struct Point: Identifiable {
        let id: Int
        let time: Date
        let value: Double
        let run: Int
    }

    var body: some View {
        let points = self.points
        let scale = ValueScale(reading)
        let values = points.map(\.value)
        let (low, high) = (values.min() ?? 0, values.max() ?? 0)
        let pad = max((high - low) * 0.2, scale.minimumSpan / 2)
        let (floor, ceiling) = (low - pad, high + pad)
        let line = LinearGradient(gradient: scale.gradient(floor, ceiling), startPoint: .bottom, endPoint: .top)
        let fill = LinearGradient(
            colors: [scale.color(high).opacity(0.32), scale.color(low).opacity(0.02)],
            startPoint: .top, endPoint: .bottom)
        let marked = scrub ?? samples.last
        Chart {
            ForEach(points) { point in
                AreaMark(
                    x: .value("Time", point.time),
                    yStart: .value("Floor", floor),
                    yEnd: .value(reading.name, point.value),
                    series: .value("Run", point.run)
                )
                .interpolationMethod(.monotone)
                .foregroundStyle(fill)
                LineMark(
                    x: .value("Time", point.time),
                    y: .value(reading.name, point.value),
                    series: .value("Run", point.run)
                )
                .interpolationMethod(.monotone)
                .lineStyle(StrokeStyle(lineWidth: 3, lineCap: .butt, lineJoin: .round))
                .foregroundStyle(line)
            }
            if let scrub {
                RuleMark(x: .value("Time", Date(timeIntervalSince1970: scrub.t)))
                    .lineStyle(StrokeStyle(lineWidth: 1))
                    .foregroundStyle(Color.primary.opacity(0.35))
            }
            if let marked {
                PointMark(x: .value("Time", Date(timeIntervalSince1970: marked.t)), y: .value(reading.name, marked.v))
                    .symbol {
                        Circle()
                            .fill(Color.white)
                            .overlay(Circle().stroke(scale.color(marked.v), lineWidth: 3))
                            .frame(width: 11, height: 11)
                            .shadow(color: .black.opacity(0.25), radius: 1.5, y: 0.5)
                    }
            }
        }
        .chartXScale(domain: now.addingTimeInterval(-24 * 3600)...now)
        .chartYScale(domain: floor...ceiling)
        .chartXAxis {
            AxisMarks(values: quarters) { value in
                AxisGridLine(stroke: StrokeStyle(lineWidth: 0.5, dash: [2, 3]))
                    .foregroundStyle(Color.primary.opacity(0.2))
                AxisValueLabel(collisionResolution: .disabled) {
                    if let date = value.as(Date.self) {
                        Text(date, format: .dateTime.hour())
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .chartYAxis {
            AxisMarks(position: .trailing, values: .automatic(desiredCount: 4)) { value in
                AxisGridLine(stroke: StrokeStyle(lineWidth: 0.5))
                    .foregroundStyle(Color.primary.opacity(0.12))
                AxisValueLabel {
                    if let number = value.as(Double.self) {
                        Text(format(number, compactUnit(reading.unit)))
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .chartLegend(.hidden)
        .chartOverlay { proxy in
            GeometryReader { geometry in
                Rectangle()
                    .fill(.clear)
                    .contentShape(Rectangle())
                    .onContinuousHover { phase in
                        guard case .active(let location) = phase,
                              let time: Date = proxy.value(atX: location.x - geometry[proxy.plotAreaFrame].origin.x)
                        else {
                            scrub = nil
                            return
                        }
                        scrub = nearest(time.timeIntervalSince1970)
                    }
            }
        }
    }

    /// Midnight, 6, noon and 18 within the day shown, as Weather marks them.
    private var quarters: [Date] {
        let calendar = Calendar.current
        let start = now.addingTimeInterval(-24 * 3600)
        let first = calendar.dateInterval(of: .day, for: start)?.start ?? start
        return (0...8)
            .compactMap { calendar.date(byAdding: .hour, value: $0 * 6, to: first) }
            .filter { $0 > start && $0 <= now }
    }

    /// Quarter-hour averages, as a smooth line draws them: the samples are
    /// stored only when they change, so drawn raw they make a staircase.
    private var points: [Point] {
        let bucket = 900.0
        let buckets = Dictionary(grouping: samples) { ($0.t / bucket).rounded(.down) }
            .sorted { $0.key < $1.key }
        var run = 0
        var previous: Double?
        return buckets.enumerated().map { index, entry in
            let (slot, members) = entry
            let time = slot * bucket + bucket / 2
            if let previous, time - previous > gap + bucket { run += 1 }
            previous = time
            let value = members.map(\.v).reduce(0, +) / Double(members.count)
            return Point(id: index, time: Date(timeIntervalSince1970: time), value: value, run: run)
        }
    }

    /// The sample nearest `time`, if the reading was measured then.
    private func nearest(_ time: Double) -> HistorySample? {
        guard let sample = samples.min(by: { abs($0.t - time) < abs($1.t - time) }) else { return nil }
        return abs(sample.t - time) <= gap ? sample : nil
    }
}

/// The last week, a row a day as Apple Weather's forecast: the low, a bar
/// from low to high on the week's scale coloured along it, and the high.
private struct WeekBars: View {
    let reading: Capability
    let samples: [HistorySample]
    let now: Date

    private struct Day: Identifiable {
        let id: Date
        let low: Double?
        let high: Double?
    }

    var body: some View {
        let days = self.days
        let scale = ValueScale(reading)
        let floor = days.compactMap(\.low).min() ?? 0
        let ceiling = max(days.compactMap(\.high).max() ?? 0, floor + scale.minimumSpan)
        VStack(spacing: 0) {
            ForEach(Array(days.enumerated()), id: \.element.id) { index, day in
                if index > 0 { Divider().opacity(0.6) }
                HStack(spacing: 10) {
                    Text(name(day.id))
                        .frame(width: 48, alignment: .leading)
                    Text(day.low.map(label) ?? "–")
                        .foregroundStyle(.secondary)
                        .frame(width: 40, alignment: .trailing)
                    RangeBar(
                        low: day.low, high: day.high, floor: floor, ceiling: ceiling,
                        now: Calendar.current.isDateInToday(day.id) ? samples.last?.v : nil,
                        scale: scale
                    )
                    .frame(height: 5)
                    Text(day.high.map(label) ?? "–")
                        .frame(width: 40, alignment: .trailing)
                }
                .font(.system(size: 13, weight: .medium))
                .monospacedDigit()
                .padding(.vertical, 6)
                .opacity(day.low == nil ? 0.35 : 1)
            }
        }
    }

    /// The last seven days, today first, as Weather lists them.
    private var days: [Day] {
        let calendar = Calendar.current
        let today = calendar.startOfDay(for: now)
        let grouped = Dictionary(grouping: samples) {
            calendar.startOfDay(for: Date(timeIntervalSince1970: $0.t))
        }
        return (0..<7).compactMap { back in
            guard let day = calendar.date(byAdding: .day, value: -back, to: today) else { return nil }
            let values = grouped[day]?.map(\.v) ?? []
            return Day(id: day, low: values.min(), high: values.max())
        }
    }

    private func name(_ day: Date) -> String {
        Calendar.current.isDateInToday(day) ? "Today" : day.formatted(.dateTime.weekday(.abbreviated))
    }

    private func label(_ value: Double) -> String {
        format(value, compactUnit(reading.unit))
    }
}

/// A day's range on the week's scale, filled with the colours it spans.
private struct RangeBar: View {
    let low: Double?
    let high: Double?
    let floor: Double
    let ceiling: Double
    let now: Double?
    let scale: ValueScale

    var body: some View {
        GeometryReader { geometry in
            let (width, height) = (geometry.size.width, geometry.size.height)
            let x = { (value: Double) in width * (value - floor) / max(ceiling - floor, .ulpOfOne) }
            ZStack(alignment: .leading) {
                Capsule().fill(Color.primary.opacity(0.1))
                if let low, let high {
                    Capsule()
                        .fill(LinearGradient(gradient: scale.gradient(low, high), startPoint: .leading, endPoint: .trailing))
                        .frame(width: max(x(high) - x(low), height))
                        .offset(x: min(x(low), width - height))
                }
                if let now {
                    let dot = height + 3
                    Circle()
                        .fill(Color.white)
                        .overlay(Circle().stroke(Color.black.opacity(0.3), lineWidth: 1.5))
                        .frame(width: dot, height: dot)
                        .offset(x: min(max(x(now) - dot / 2, 0), width - dot))
                }
            }
        }
    }
}

/// Short units for charts: 21.4° rather than 21.4 °C.
private func compactUnit(_ unit: String?) -> String? {
    switch unit {
    case "°C": return "°"
    case "%": return "%"
    default: return nil
    }
}

/// Colours for a reading's values, like Apple Weather's temperature colours.
/// Chosen by unit, and for percentages by what is measured, so any device's
/// readings get them; anything else takes the accent colour.
struct ValueScale {
    private let stops: [(value: Double, color: NSColor)]
    /// The narrowest range worth drawing, so a steady reading is not magnified.
    let minimumSpan: Double

    init(_ reading: Capability) {
        let rgb = { (red: CGFloat, green: CGFloat, blue: CGFloat) in
            NSColor(srgbRed: red, green: green, blue: blue, alpha: 1)
        }
        let (blue, teal, green) = (rgb(0.33, 0.55, 1.0), rgb(0.30, 0.76, 0.90), rgb(0.33, 0.80, 0.47))
        let (yellow, orange, red) = (rgb(0.99, 0.80, 0.24), rgb(1.0, 0.56, 0.20), rgb(0.95, 0.27, 0.25))
        switch reading.unit ?? "" {
        case "°C":
            stops = [(-5, blue), (8, teal), (16, green), (22, yellow), (28, orange), (35, red)]
            minimumSpan = 2
        case "%" where reading.id.contains("humid"):
            stops = [(20, orange), (35, yellow), (50, green), (65, teal), (85, blue)]
            minimumSpan = 10
        case "µg/m³":
            stops = [(0, green), (12, yellow), (35, orange), (55, red), (150, rgb(0.62, 0.32, 0.78))]
            minimumSpan = 10
        case "mg/m³":
            stops = [(0, green), (0.05, yellow), (0.1, orange), (0.3, red)]
            minimumSpan = 0.05
        default:
            stops = [(0, NSColor.controlAccentColor)]
            minimumSpan = 1
        }
    }

    func color(_ value: Double) -> Color {
        Color(nsColor: nsColor(value))
    }

    /// The colours from `low` to `high`, for a gradient across that range.
    func gradient(_ low: Double, _ high: Double) -> Gradient {
        let steps = 8
        return Gradient(stops: (0...steps).map { step in
            let fraction = Double(step) / Double(steps)
            return .init(color: color(low + (high - low) * fraction), location: fraction)
        })
    }

    private func nsColor(_ value: Double) -> NSColor {
        guard let first = stops.first, let last = stops.last else { return .controlAccentColor }
        if value <= first.value { return first.color }
        if value >= last.value { return last.color }
        let upper = stops.firstIndex { $0.value >= value } ?? stops.count - 1
        let (below, above) = (stops[upper - 1], stops[upper])
        let fraction = (value - below.value) / (above.value - below.value)
        return below.color.blended(withFraction: fraction, of: above.color) ?? below.color
    }
}
