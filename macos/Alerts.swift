import Foundation
import UserNotifications

/// Posts a Mac notification when a device raises an alert: a filter to
/// replace, salt to refill, a programme finished.
///
/// Once when an alert appears, then not again while it holds. One that comes
/// back within half an hour is the same one, after a restart or a reading
/// near its threshold. What held is remembered across launches, so a worn
/// filter is not announced every time the app starts.
final class Notifier {
    private static let settle: TimeInterval = 30 * 60
    private static let holdingKey = "alerts.holding"
    private static let clearedKey = "alerts.cleared"

    private let center = UNUserNotificationCenter.current()
    private let defaults = UserDefaults.standard
    private var holding: Set<String>
    /// When each alert last cleared, in Unix seconds.
    private var cleared: [String: Double]

    init() {
        holding = Set(defaults.stringArray(forKey: Self.holdingKey) ?? [])
        cleared = defaults.dictionary(forKey: Self.clearedKey) as? [String: Double] ?? [:]
        center.requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    /// The devices as the bridge last reported them.
    func see(_ devices: [Device]) {
        let now = Date().timeIntervalSince1970
        var present: Set<String> = []
        for device in devices {
            for alert in device.alerts ?? [] {
                let id = "\(device.id)/\(alert.id)"
                present.insert(id)
                if !holding.contains(id), now - (cleared[id] ?? 0) > Self.settle {
                    post(id, title: device.name, body: alert.message)
                }
            }
        }
        guard present != holding else { return }
        for id in holding.subtracting(present) {
            cleared[id] = now
        }
        holding = present
        defaults.set(Array(holding), forKey: Self.holdingKey)
        defaults.set(cleared, forKey: Self.clearedKey)
    }

    private func post(_ id: String, title: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        center.add(UNNotificationRequest(identifier: id, content: content, trigger: nil))
    }
}
