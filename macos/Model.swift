import AppKit
import Darwin
import Foundation

/// The menu bar's view of the bridge, refreshed from its control socket.
final class BridgeModel: ObservableObject {
    @Published var status: Status?
    @Published var running = false
    @Published var missingBinary = false
    @Published var copied = false
    @Published var starting = false
    /// Between Forget Home and the restarted bridge saying it is unpaired.
    @Published var forgetting = false
    /// The last refusal from a device, shown under the devices for a moment.
    @Published var problem: String?
    /// Why the bridge stopped right after Start, from the end of its log.
    @Published var startFailure: String?

    private let queue = DispatchQueue(label: "hearth.control")
    /// Devices arrive as they change, from the bridge's stream. While it is
    /// up, the rest of the status (the group, Home) is asked for less often.
    private var streaming = false
    private var lastAsked = Date.distantPast
    private let notifier = Notifier()
    private var timer: Timer?

    /// Whether a Home holds the bridge. Unknown while it is not running.
    var paired: Bool? {
        running ? status?.paired : nil
    }

    init() {
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            guard let self else { return }
            if !self.streaming || Date().timeIntervalSince(self.lastAsked) >= 5 {
                self.refresh()
            }
        }
        follow()
        // After an update, the new app starts the bridge the old one stopped;
        // and when asked to, every time Hearth opens, as at login.
        if CommandLine.arguments.contains(Updater.restartFlag) || Launch.startsBridge {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
                if self?.running == false { self?.start() }
            }
        }
    }

    /// Follows the bridge's stream of changes on a thread of its own, coming
    /// back to it when the bridge restarts.
    private func follow() {
        Thread.detachNewThread { [weak self] in
            while self != nil {
                try? ControlSocket.subscribe(["op": "subscribe"]) { line in
                    guard let change = try? JSONDecoder().decode(StateChange.self, from: line) else {
                        return
                    }
                    DispatchQueue.main.async { self?.apply(change) }
                }
                DispatchQueue.main.async { self?.streaming = false }
                Thread.sleep(forTimeInterval: 3)
            }
        }
    }

    private func apply(_ change: StateChange) {
        streaming = true
        guard var devices = status?.devices else { return }
        switch change.device {
        case .device(let device):
            if let index = devices.firstIndex(where: { $0.id == device.id }) {
                devices[index] = device
            } else {
                devices.append(device)
            }
        case .id(let id) where change.event == "removed":
            devices.removeAll { $0.id == id }
        case .id:
            return
        }
        status?.devices = devices
    }

    /// Reads the bridge's state off the main thread, so the panel never waits.
    func refresh() {
        lastAsked = Date()
        queue.async { [weak self] in
            let status = (try? ControlSocket.request(["op": "status"]))
                .flatMap { try? JSONDecoder().decode(Status.self, from: $0) }
            DispatchQueue.main.async {
                guard let self else { return }
                self.status = status
                self.running = status != nil
                if let status { self.notifier.see(status.devices) }
                if self.forgetting && self.paired == false {
                    self.forgetting = false
                }
            }
        }
    }

    /// Shows the change at once, then asks the bridge. A refusal puts the
    /// device's real state back on the next refresh.
    func set(_ device: Device, _ capability: Capability, _ value: JSONValue) {
        if let d = status?.devices.firstIndex(where: { $0.id == device.id }),
           let c = status?.devices[d].capabilities.firstIndex(where: { $0.id == capability.id }),
           !["keys", "text"].contains(capability.kind)
        {
            status?.devices[d].capabilities[c].value = value
        }
        send(["op": "set", "device": device.id, "capability": capability.id, "value": value.json])
    }

    /// Renames a device here at once; the bridge renames it in Home.
    func rename(_ device: Device, _ name: String) {
        let name = name.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, name != device.name else { return }
        if let d = status?.devices.firstIndex(where: { $0.id == device.id }) {
            status?.devices[d].name = name
        }
        send(["op": "rename", "device": device.id, "name": name])
    }

    /// Acts on the group: approve, decline, confirm, separate or join.
    func group(_ op: String, code: String? = nil) {
        var body: [String: Any] = ["op": op]
        body["code"] = code
        send(body)
    }

    /// Asks the bridge, and shows a refusal for a moment.
    private func send(_ body: [String: Any]) {
        queue.async { [weak self] in
            var message: String?
            do {
                _ = try ControlSocket.request(body)
            } catch let failure as ControlSocket.Failure {
                message = failure.message
            } catch {
                message = error.localizedDescription
            }
            DispatchQueue.main.async {
                self?.problem = message
                if message != nil {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 4) { [weak self] in
                        if self?.problem == message { self?.problem = nil }
                    }
                }
                self?.refresh()
            }
        }
    }

    func hearthURL() -> URL? {
        if let bundled = Bundle.main.url(forAuxiliaryExecutable: "hearth-cli"),
           FileManager.default.isExecutableFile(atPath: bundled.path)
        {
            return bundled
        }
        let candidates = [
            FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".cargo/bin/hearth"),
            URL(fileURLWithPath: "/usr/local/bin/hearth"),
            URL(fileURLWithPath: "/opt/homebrew/bin/hearth"),
        ]
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    /// Where `start` sends the bridge's output. It starts over with each run,
    /// so it cannot grow without end.
    static let logURL = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".config/hearth/hearth.log")

    func start() {
        guard !starting, let url = hearthURL() else {
            if hearthURL() == nil { missingBinary = true }
            return
        }
        missingBinary = false
        startFailure = nil
        starting = true
        // On a new Mac there is no ~/.config/hearth yet, and the shell will
        // not run a command whose log it cannot create.
        try? FileManager.default.createDirectory(
            at: Self.logURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let shell = Process()
        shell.executableURL = URL(fileURLWithPath: "/bin/sh")
        let path = url.path.replacingOccurrences(of: "'", with: "'\\''")
        let log = Self.logURL.path.replacingOccurrences(of: "'", with: "'\\''")
        shell.arguments = ["-c", "nohup '\(path)' run >'\(log)' 2>&1 &"]
        shell.standardOutput = FileHandle.nullDevice
        shell.standardError = FileHandle.nullDevice
        do {
            try shell.run()
            shell.waitUntilExit()
        } catch {
            startFailure = "Could not start the bridge: \(error.localizedDescription)"
            starting = false
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
            self?.starting = false
        }
        // A bridge that stopped at once says why at the end of its log.
        DispatchQueue.main.asyncAfter(deadline: .now() + 6) { [weak self] in
            guard let self, !self.running else { return }
            self.startFailure = Self.lastLogLine()
                ?? "The bridge stopped as it started. See ~/.config/hearth/hearth.log."
        }
    }

    /// The bridge's error, the line after "Error:" when it printed one.
    private static func lastLogLine() -> String? {
        guard let text = try? String(contentsOf: logURL, encoding: .utf8) else { return nil }
        let lines = text.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }
        if let error = lines.first(where: { $0.hasPrefix("Error:") }) {
            return String(error.dropFirst("Error:".count)).trimmingCharacters(in: .whitespaces)
        }
        return lines.last { !$0.isEmpty }
    }

    func stop() {
        guard let pid = status?.pid, pid > 0 else { return }
        kill(Int32(pid), SIGTERM)
    }

    /// Forgets Home's pairing and restarts the bridge, so it shows up in Add
    /// Accessory again. For a bridge removed from Home while it was stopped.
    func forgetHome() {
        guard let url = hearthURL() else {
            missingBinary = true
            return
        }
        stop()
        starting = true
        forgetting = true
        // A bridge that never comes back should not leave the card spinning.
        DispatchQueue.main.asyncAfter(deadline: .now() + 20) { [weak self] in
            self?.forgetting = false
        }
        // The bridge re-advertises only when it starts, so stop it first.
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
            let unpair = Process()
            unpair.executableURL = url
            unpair.arguments = ["unpair"]
            unpair.standardOutput = FileHandle.nullDevice
            unpair.standardError = FileHandle.nullDevice
            try? unpair.run()
            unpair.waitUntilExit()
            self?.starting = false
            self?.start()
        }
    }

    func copyPin() {
        guard let pin = status?.pin, !pin.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(pin, forType: .string)
        copied = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { [weak self] in
            self?.copied = false
        }
    }
}
