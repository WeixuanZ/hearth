import AppKit
import Darwin
import Foundation

struct Snapshot: Codable {
    var pid: Int
    var updated: UInt64
    var name: String
    var pin: String
    var devices: [DeviceSnap]
}

struct DeviceSnap: Codable, Identifiable {
    var id: String
    var kind: String
    var name: String
    var online: Bool
    var headline: String
    var detail: String
    var metrics: [Metric]
    var seen: UInt64
}

struct Metric: Codable, Identifiable {
    var label: String
    var value: String
    var id: String { label }
}

final class BridgeModel: ObservableObject {
    @Published var snapshot: Snapshot?
    @Published var running = false
    @Published var missingBinary = false
    @Published var copied = false
    @Published var starting = false

    private var timer: Timer?

    init() {
        tick()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            self?.tick()
        }
    }

    func tick() {
        let url = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config/hearth/status.json")
        guard let data = try? Data(contentsOf: url),
              let snap = try? JSONDecoder().decode(Snapshot.self, from: data)
        else {
            snapshot = nil
            running = false
            return
        }
        snapshot = snap
        let alive = snap.pid > 0 && kill(Int32(snap.pid), 0) == 0
        let age = Date().timeIntervalSince1970 - TimeInterval(snap.updated)
        running = alive && age < 15
    }

    func hearthURL() -> URL? {
        if let bundled = Bundle.main.url(forAuxiliaryExecutable: "hearth"),
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

    func start() {
        guard !starting, let url = hearthURL() else {
            if hearthURL() == nil { missingBinary = true }
            return
        }
        missingBinary = false
        starting = true
        let shell = Process()
        shell.executableURL = URL(fileURLWithPath: "/bin/sh")
        let path = url.path.replacingOccurrences(of: "'", with: "'\\''")
        shell.arguments = ["-c", "nohup '\(path)' run >/dev/null 2>&1 &"]
        shell.standardOutput = FileHandle.nullDevice
        shell.standardError = FileHandle.nullDevice
        do {
            try shell.run()
            shell.waitUntilExit()
        } catch {
            missingBinary = true
            starting = false
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
            self?.starting = false
        }
    }

    func stop() {
        guard let pid = snapshot?.pid, pid > 0 else { return }
        kill(Int32(pid), SIGTERM)
    }

    func copyPin() {
        guard let pin = snapshot?.pin, !pin.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(pin, forType: .string)
        copied = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { [weak self] in
            self?.copied = false
        }
    }
}
