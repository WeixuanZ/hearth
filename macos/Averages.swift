import Charts
import SwiftUI

/// A reading's usual day, from the bridge: each hour's range over the last
/// thirty days and its mean, with today so far.
struct NormalsReply: Decodable {
    struct Hour: Decodable, Identifiable {
        var hour: Int
        var low: Double
        var mean: Double
        var high: Double
        var id: Int { hour }
    }

    var days: Int
    var normals: [Hour]
    var today: [HistorySample]
}

/// Today against the usual day, as Apple Weather's Averages: how far from
/// the average it is now, today's line over the usual range, and a summary.
struct AveragesView: View {
    let reading: Capability
    let reply: NormalsReply
    let now: Date

    private var midnight: Date { Calendar.current.startOfDay(for: now) }
    private var scale: ValueScale { ValueScale(reading) }
    private var current: Double? { reply.today.last?.v ?? reading.value.number }
    private var usual: NormalsReply.Hour? {
        let hour = Calendar.current.component(.hour, from: now)
        return reply.normals.first { $0.hour == hour }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            headline
            if reply.normals.isEmpty {
                Text("A usual day needs three days of readings. Hearth has \(reply.days).")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 150)
            } else {
                chart.frame(height: 150)
                legend
                Text(summary)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var headline: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(reading.name.uppercased())
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.secondary)
            if let current, let usual {
                Text(difference(current - usual.mean))
                    .font(.system(size: 24, weight: .semibold, design: .rounded))
                    .monospacedDigit()
                Text("Usually \(label(usual.mean)) at this hour")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            } else {
                Text("Averages")
                    .font(.system(size: 24, weight: .semibold, design: .rounded))
            }
        }
    }

    private var chart: some View {
        let today = todayPoints
        let values = reply.normals.flatMap { [$0.low, $0.high] } + today.map(\.value)
        let (low, high) = (values.min() ?? 0, values.max() ?? 0)
        let pad = max((high - low) * 0.15, scale.minimumSpan / 2)
        let (floor, ceiling) = (low - pad, high + pad)
        let line = LinearGradient(gradient: scale.gradient(floor, ceiling), startPoint: .bottom, endPoint: .top)
        return Chart {
            ForEach(reply.normals) { hour in
                AreaMark(
                    x: .value("Time", at(hour.hour)),
                    yStart: .value("Low", hour.low),
                    yEnd: .value("High", hour.high),
                    series: .value("Series", "usual")
                )
                .interpolationMethod(.monotone)
                .foregroundStyle(line.opacity(0.3))
            }
            ForEach(today) { point in
                LineMark(
                    x: .value("Time", point.time),
                    y: .value(reading.name, point.value),
                    series: .value("Series", "today")
                )
                .interpolationMethod(.monotone)
                .lineStyle(StrokeStyle(lineWidth: 3, lineCap: .round, lineJoin: .round))
                .foregroundStyle(line)
            }
            if let last = today.last {
                PointMark(x: .value("Time", last.time), y: .value(reading.name, last.value))
                    .symbol {
                        Circle()
                            .fill(Color(nsColor: .windowBackgroundColor))
                            .overlay(Circle().stroke(scale.color(last.value), lineWidth: 3))
                            .frame(width: 11, height: 11)
                    }
            }
        }
        .chartXScale(domain: midnight...midnight.addingTimeInterval(24 * 3600))
        .chartYScale(domain: floor...ceiling)
        .chartXAxis {
            AxisMarks(values: (0..<4).map { at($0 * 6, middle: false) }) { value in
                AxisGridLine(stroke: StrokeStyle(lineWidth: 0.5, dash: [2, 3]))
                    .foregroundStyle(Color.primary.opacity(0.2))
                AxisValueLabel(collisionResolution: .disabled) {
                    if let date = value.as(Date.self) {
                        Text(date, format: .dateTime.hour(.twoDigits(amPM: .omitted)))
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
                        Text(label(number))
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
    }

    private var legend: some View {
        let range = (reply.normals.map(\.low).min() ?? 0, reply.normals.map(\.high).max() ?? 0)
        let mean = reply.normals.map(\.mean).reduce(0, +) / Double(max(reply.normals.count, 1))
        return HStack(spacing: 16) {
            Label {
                Text("Today")
            } icon: {
                Circle().fill(scale.color(current ?? mean)).frame(width: 9, height: 9)
            }
            Label {
                Text("Usual range (\(label(range.0)) to \(label(range.1)))")
            } icon: {
                Circle().fill(scale.color(mean).opacity(0.45)).frame(width: 9, height: 9)
            }
        }
        .font(.system(size: 11))
        .foregroundStyle(.secondary)
    }

    private var summary: String {
        let days = reply.days == 1 ? "day" : "\(reply.days) days"
        guard let usual else {
            return "From the last \(days) of readings."
        }
        let name = reading.name.lowercased()
        var text = "Over the last \(days), the \(name) at this hour has usually been "
            + "\(label(usual.low)) to \(label(usual.high)), averaging \(label(usual.mean))."
        if let current {
            text += " It is \(label(current)) now."
        }
        return text
    }

    private struct Point: Identifiable {
        let time: Date
        let value: Double
        var id: Date { time }
    }

    /// Quarter-hour means, as the day chart draws them.
    private var todayPoints: [Point] {
        let bucket = 900.0
        return Dictionary(grouping: reply.today) { ($0.t / bucket).rounded(.down) }
            .sorted { $0.key < $1.key }
            .map { slot, members in
                Point(time: Date(timeIntervalSince1970: slot * bucket + bucket / 2),
                      value: members.map(\.v).reduce(0, +) / Double(members.count))
            }
    }

    /// The hour on today's clock; the middle of it, where its mean belongs.
    private func at(_ hour: Int, middle: Bool = true) -> Date {
        midnight.addingTimeInterval(Double(hour) * 3600 + (middle ? 1800 : 0))
    }

    private func label(_ value: Double) -> String {
        format(value, reading.unit == "°C" ? "°" : reading.unit)
    }

    /// "+1.2° from average", or "Same as average" when it rounds to nothing.
    private func difference(_ delta: Double) -> String {
        let rounded = (delta * 10).rounded() / 10
        if rounded == 0 { return "Same as average" }
        return "\(rounded > 0 ? "+" : "−")\(label(abs(rounded))) from average"
    }
}
