import AppKit
import Darwin
import Foundation
import ServiceManagement

struct Snapshot: Codable {
    var pid: Int
    var updated: UInt64
    var name: String
    var pin: String
    var paired: Bool?
    var role: String?
    var leader: String?
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
    /// Between Forget Home and the restarted bridge saying it is unpaired.
    @Published var forgetting = false
    @Published var adding = false
    @Published var products: [Product] = []
    @Published var notice: String?
    @Published var busy = false
    @Published var updateTag: String?
    @Published var updating = false
    @Published var loginItem = false

    private var timer: Timer?
    private var updateTimer: Timer?

    /// Whether a Home holds the bridge. Unknown while it is not running.
    var paired: Bool? {
        running ? snapshot?.paired : nil
    }

    init() {
        tick()
        loadProducts()
        refreshLogin()
        checkForUpdate()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            self?.tick()
        }
        updateTimer = Timer.scheduledTimer(withTimeInterval: 6 * 60 * 60, repeats: true) { [weak self] _ in
            self?.checkForUpdate()
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
            self?.tick()
            if self?.running == false {
                self?.start()
            }
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
        if forgetting && paired == false {
            forgetting = false
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
        // The log starts over with each run, so it cannot grow without end.
        let log = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config/hearth/hearth.log").path
            .replacingOccurrences(of: "'", with: "'\\''")
        shell.arguments = ["-c", "nohup '\(path)' run >'\(log)' 2>&1 &"]
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
        guard let pin = snapshot?.pin, !pin.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(pin, forType: .string)
        copied = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { [weak self] in
            self?.copied = false
        }
    }

    func loadProducts() {
        let (code, text) = run(["products"], input: nil)
        guard code == 0, let data = text.data(using: .utf8),
              let list = try? JSONDecoder().decode([Product].self, from: data)
        else { return }
        products = list
    }

    func removeDevice(_ name: String) {
        runAsync(["remove", name]) { [weak self] code, text in
            self?.notice = code == 0 ? nil : text
        }
    }

    func requestDyson(email: String, country: String, done: @escaping (String?) -> Void) {
        busy = true
        runAsync(["dyson", "request", "--email", email, "--country", country], input: nil) { [weak self] code, text in
            self?.busy = false
            done(code == 0 ? nil : text)
        }
    }

    func submitDyson(password: String, code: String, done: @escaping (String?) -> Void) {
        busy = true
        runAsync(["dyson", "submit"], input: password + "\n" + code + "\n") { [weak self] code, text in
            self?.busy = false
            self?.adding = code != 0
            done(code == 0 ? nil : text)
        }
    }

    func pairFrame(name: String, host: String, done: @escaping (String?) -> Void) {
        busy = true
        var args = ["frame", "pair", "--name", name]
        if !host.isEmpty { args += ["--host", host] }
        runAsync(args, input: nil) { [weak self] code, text in
            self?.busy = false
            self?.adding = code != 0
            done(code == 0 ? nil : text)
        }
    }

    func refreshLogin() {
        loginItem = SMAppService.mainApp.status == .enabled
    }

    func setLogin(_ on: Bool) {
        do {
            if on {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            notice = nil
        } catch {
            notice = error.localizedDescription
        }
        refreshLogin()
    }

    func checkForUpdate() {
        guard let url = URL(string: "https://api.github.com/repos/WeixuanZ/hearth/releases/latest") else { return }
        var request = URLRequest(url: url)
        request.setValue("hearth", forHTTPHeaderField: "User-Agent")
        URLSession.shared.dataTask(with: request) { [weak self] data, response, _ in
            guard let data, let http = response as? HTTPURLResponse, http.statusCode == 200,
                  let release = try? JSONDecoder().decode(GitHubRelease.self, from: data)
            else { return }
            let current = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.2.0"
            let latest = release.tag_name.trimmingCharacters(in: CharacterSet(charactersIn: "v"))
            DispatchQueue.main.async {
                self?.updateTag = Self.version(latest, isNewerThan: current) ? release.tag_name : nil
            }
        }.resume()
    }

    func installUpdate() {
        guard let tag = updateTag, !updating else { return }
        guard Bundle.main.bundleURL.pathExtension == "app" else {
            notice = "Open Hearth.app to update it."
            return
        }
        updating = true
        notice = nil
        let dest = Bundle.main.bundleURL
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let error = Self.downloadAndReplace(tag: tag, dest: dest)
            DispatchQueue.main.async {
                self?.updating = false
                if let error {
                    self?.notice = error
                }
            }
        }
    }

    private func runAsync(_ args: [String], input: String? = nil, done: @escaping (Int32, String) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let result = self?.run(args, input: input) ?? (1, "Hearth is not installed")
            DispatchQueue.main.async { done(result.0, result.1) }
        }
    }

    private func run(_ args: [String], input: String?) -> (Int32, String) {
        guard let url = hearthURL() else { return (1, "Hearth is not installed") }
        let process = Process()
        process.executableURL = url
        process.arguments = args
        let output = Pipe()
        let errors = Pipe()
        process.standardOutput = output
        process.standardError = errors
        if let input {
            let stdin = Pipe()
            process.standardInput = stdin
            do { try process.run() } catch { return (1, error.localizedDescription) }
            stdin.fileHandleForWriting.write(Data(input.utf8))
            try? stdin.fileHandleForWriting.close()
        } else {
            do { try process.run() } catch { return (1, error.localizedDescription) }
        }
        process.waitUntilExit()
        let text = String(data: output.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let err = String(data: errors.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let combined = (text + err).trimmingCharacters(in: .whitespacesAndNewlines)
        return (process.terminationStatus, combined)
    }

    private static func version(_ latest: String, isNewerThan current: String) -> Bool {
        let a = latest.split(separator: ".").compactMap { Int($0) }
        let b = current.split(separator: ".").compactMap { Int($0) }
        let count = max(a.count, b.count)
        for index in 0..<count {
            let left = index < a.count ? a[index] : 0
            let right = index < b.count ? b[index] : 0
            if left != right { return left > right }
        }
        return false
    }

    private static func downloadAndReplace(tag: String, dest: URL) -> String? {
        #if arch(arm64)
        let arch = "arm64"
        #else
        let arch = "x86_64"
        #endif
        let asset = "Hearth-\(tag)-macos-\(arch).zip"
        let base = "https://github.com/WeixuanZ/hearth/releases/download/\(tag)/"
        guard let zipURL = URL(string: base + asset),
              let sumsURL = URL(string: base + "SHA256SUMS")
        else { return "Could not build the download address." }
        let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("hearth-update-\(UUID().uuidString)")
        do {
            try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
            let zip = tmp.appendingPathComponent(asset)
            try Data(contentsOf: zipURL).write(to: zip)
            let sums = String(decoding: Data(contentsOf: sumsURL), as: UTF8.self)
            guard let expected = sums.split(separator: "\n").compactMap({ line -> String? in
                let parts = line.split(separator: " ")
                guard let hash = parts.first, parts.contains(where: { $0.hasSuffix(Substring(asset)) }) else { return nil }
                return String(hash)
            }).first else { return "The release has no checksum for this Mac." }
            let got = try checksum(zip)
            guard got == expected else { return "The download did not match its checksum." }
            let unpack = tmp.appendingPathComponent("unpack")
            try FileManager.default.createDirectory(at: unpack, withIntermediateDirectories: true)
            let ditto = Process()
            ditto.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
            ditto.arguments = ["-x", "-k", zip.path, unpack.path]
            try ditto.run()
            ditto.waitUntilExit()
            guard ditto.terminationStatus == 0 else { return "Could not open the update." }
            let app = unpack.appendingPathComponent("Hearth.app")
            guard FileManager.default.fileExists(atPath: app.path) else { return "The update did not contain the app." }
            let pid = ProcessInfo.processInfo.processIdentifier
            let script = """
            while kill -0 \(pid) 2>/dev/null; do sleep 0.2; done
            rm -rf '\(dest.path.replacingOccurrences(of: "'", with: "'\\''"))'
            mv '\(app.path.replacingOccurrences(of: "'", with: "'\\''"))' '\(dest.path.replacingOccurrences(of: "'", with: "'\\''"))'
            open '\(dest.path.replacingOccurrences(of: "'", with: "'\\''"))'
            """
            let shell = Process()
            shell.executableURL = URL(fileURLWithPath: "/bin/sh")
            shell.arguments = ["-c", script]
            try shell.run()
            DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    private static func checksum(_ url: URL) throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/shasum")
        process.arguments = ["-a", "256", url.path]
        let pipe = Pipe()
        process.standardOutput = pipe
        try process.run()
        process.waitUntilExit()
        let text = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return text.split(separator: " ").first.map(String.init) ?? ""
    }
}

struct Product: Codable, Identifiable {
    var id: String
    var kind: String
    var name: String
}

private struct GitHubRelease: Codable {
    var tag_name: String
}
