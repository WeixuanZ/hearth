import AppKit
import CryptoKit
import Foundation

/// Keeps the app current: looks for a newer GitHub release, and installs
/// one on request. The download for this Mac is checked against the
/// release's SHA256SUMS, unpacked, and swapped in once this app quits.
final class Updater: ObservableObject {
    enum State: Equatable {
        case current
        case available(String)
        case installing(String)
        case failed(String)
    }

    /// Tells the new app to start the bridge the old one stopped.
    static let restartFlag = "--start-bridge"
    private static let repository = "WeixuanZ/hearth"

    @Published var state = State.current
    private var timer: Timer?

    let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"

    init() {
        check()
        timer = Timer.scheduledTimer(withTimeInterval: 6 * 60 * 60, repeats: true) { [weak self] _ in
            self?.check()
        }
    }

    func check() {
        guard Bundle.main.bundleURL.pathExtension == "app" else { return }
        Task {
            guard let tag = try? await Self.latestTag(),
                  Self.version(tag, isNewerThan: version)
            else { return }
            await MainActor.run {
                if case .installing = state { return }
                state = .available(tag)
            }
        }
    }

    /// Downloads and checks the release, then quits so it can take this
    /// app's place. The bridge restarts from the new app if it was running.
    func install(_ tag: String, model: BridgeModel) {
        let app = Bundle.main.bundleURL
        guard FileManager.default.isWritableFile(atPath: app.deletingLastPathComponent().path) else {
            state = .failed("Hearth cannot replace itself where it is. Move it to Applications.")
            return
        }
        state = .installing(tag)
        let restart = model.running
        Task {
            do {
                let next = try await Self.download(tag)
                await MainActor.run {
                    if restart { model.stop() }
                    Self.swap(app, for: next, restart: restart)
                    NSApplication.shared.terminate(nil)
                }
            } catch {
                await MainActor.run { state = .failed(error.localizedDescription) }
            }
        }
    }

    private struct Failure: LocalizedError {
        let errorDescription: String?
        init(_ message: String) { errorDescription = message }
    }

    private static func latestTag() async throws -> String {
        struct Release: Decodable { let tag_name: String }
        var request = URLRequest(url: URL(string: "https://api.github.com/repos/\(repository)/releases/latest")!)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        let (data, _) = try await URLSession.shared.data(for: request)
        return try JSONDecoder().decode(Release.self, from: data).tag_name
    }

    /// The new Hearth.app, unpacked in a temporary folder.
    private static func download(_ tag: String) async throws -> URL {
        #if arch(arm64)
        let arch = "arm64"
        #else
        let arch = "x86_64"
        #endif
        let asset = "Hearth-\(tag)-macos-\(arch).zip"
        let base = URL(string: "https://github.com/\(repository)/releases/download/\(tag)/")!
        let (sums, _) = try await URLSession.shared.data(from: base.appendingPathComponent("SHA256SUMS"))
        let expected = String(decoding: sums, as: UTF8.self)
            .split(separator: "\n")
            .map { $0.split(separator: " ", omittingEmptySubsequences: true) }
            .first { $0.count == 2 && $0[1].hasSuffix(asset) }?
            .first
        guard let expected else { throw Failure("The release has nothing for this Mac.") }

        let (zip, _) = try await URLSession.shared.download(from: base.appendingPathComponent(asset))
        let digest = SHA256.hash(data: try Data(contentsOf: zip, options: .mappedIfSafe))
        let hex = digest.map { String(format: "%02x", $0) }.joined()
        guard hex == expected else { throw Failure("The download did not match its checksum.") }

        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("hearth-\(tag)-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let ditto = Process()
        ditto.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
        ditto.arguments = ["-x", "-k", zip.path, folder.path]
        try ditto.run()
        ditto.waitUntilExit()
        let app = folder.appendingPathComponent("Hearth.app")
        let unpacked = Bundle(url: app)?.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        guard ditto.terminationStatus == 0, unpacked.map({ "v\($0)" == tag }) == true else {
            throw Failure("The update could not be opened.")
        }
        return app
    }

    /// Once this process has gone: moves `next` into `app`'s place, putting
    /// the old one back if that fails, and opens it.
    private static func swap(_ app: URL, for next: URL, restart: Bool) {
        let script = """
        while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
        mv "$2" "$2.old" && { mv "$3" "$2" || mv "$2.old" "$2"; }
        rm -rf "$2.old"
        open "$2" --args $4
        """
        let shell = Process()
        shell.executableURL = URL(fileURLWithPath: "/bin/sh")
        shell.arguments = [
            "-c", script, "sh", String(ProcessInfo.processInfo.processIdentifier),
            app.path, next.path, restart ? restartFlag : "",
        ]
        try? shell.run()
    }

    static func version(_ tag: String, isNewerThan current: String) -> Bool {
        let parts = { (text: String) in
            text.trimmingCharacters(in: CharacterSet(charactersIn: "v"))
                .split(separator: ".").map { Int($0) ?? 0 }
        }
        let (new, old) = (parts(tag), parts(current))
        for index in 0..<max(new.count, old.count) {
            let (a, b) = (index < new.count ? new[index] : 0, index < old.count ? old[index] : 0)
            if a != b { return a > b }
        }
        return false
    }
}
