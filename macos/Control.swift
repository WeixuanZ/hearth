import Darwin
import Foundation

/// The bridge's state, as `hearth devices --json` prints it.
struct Status: Decodable {
    var pid: Int
    var name: String
    var pin: String
    var paired: Bool
    var devices: [Device]
    var group: GroupStatus?
}

/// The computers sharing the bridge, from this one's point of view.
struct GroupStatus: Decodable {
    /// This computer.
    var name: String
    /// serving, standby, joining, looking, or conflict.
    var role: String
    /// Who serves Home, or who this computer is asking to join.
    var leader: String?
    var members: [String]
    /// A computer asking to join this one.
    var asking: String?
    /// The code this computer shows while it asks to join.
    var code: String?
    /// A computer in another group on the network.
    var other: String?

    var serving: Bool { role == "serving" || role == "conflict" }
}

struct Device: Decodable, Identifiable {
    var id: String
    var kind: String
    var category: String
    var name: String
    var online: Bool
    var headline: String
    var detail: String
    var capabilities: [Capability]
    /// Missing from a bridge older than alerts.
    var alerts: [Alert]?
}

/// Something that needs a person, like a filter to replace.
struct Alert: Decodable {
    var id: String
    var message: String
}

/// One thing a device can do or report. Which fields are set depends on
/// `kind`: power, switch, choice, level, reading or keys.
struct Capability: Decodable, Identifiable {
    var id: String
    var name: String
    var kind: String
    var value: JSONValue
    var active: Bool
    var options: [Option]?
    var keys: [Option]?
    var min: Double?
    var max: Double?
    var step: Double?
    var unit: String?
    /// A run's: why it cannot start or stop now, seconds left, percent done.
    var blocked: String?
    var remaining: Double?
    var progress: Double?
}

struct Option: Decodable, Identifiable, Hashable {
    var id: String
    var name: String
}

/// A capability's value: a bool, a number, text, or nothing yet.
enum JSONValue: Decodable, Equatable {
    case bool(Bool)
    case number(Double)
    case text(String)
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let bool = try? container.decode(Bool.self) {
            self = .bool(bool)
        } else if let number = try? container.decode(Double.self) {
            self = .number(number)
        } else {
            self = .text(try container.decode(String.self))
        }
    }

    var bool: Bool { if case .bool(let bool) = self { return bool } else { return false } }
    var number: Double? { if case .number(let number) = self { return number } else { return nil } }
    var text: String? { if case .text(let text) = self { return text } else { return nil } }

    var json: Any {
        switch self {
        case .bool(let bool): return bool
        case .number(let number): return number
        case .text(let text): return text
        case .null: return NSNull()
        }
    }
}

/// A reading over time, from `{"op":"history"}`. `t` is Unix seconds.
struct HistorySample: Decodable {
    var t: Double
    var v: Double
}

struct HistorySeries: Decodable {
    var capability: String
    var name: String
    var unit: String
    var samples: [HistorySample]
}

struct HistoryReply: Decodable {
    var interval: Int
    var heartbeat: Int
    var series: [HistorySeries]
}

/// `~/.config/hearth/control.sock`: one JSON line out, one back. Calls block,
/// so make them off the main thread.
enum ControlSocket {
    static let path = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".config/hearth/control.sock").path

    struct Failure: Error {
        var message: String
    }

    /// Sends one request and waits for its reply, `wait` seconds at most:
    /// longer for a step that waits on a person, like Allow on a TV.
    static func request(_ body: [String: Any], wait: Int = 5) throws -> Data {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw Failure(message: "no socket") }
        defer { close(fd) }

        var timeout = timeval(tv_sec: wait, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))

        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: address.sun_path) else {
            throw Failure(message: "socket path too long")
        }
        withUnsafeMutableBytes(of: &address.sun_path) { buffer in
            buffer.copyBytes(from: bytes)
        }
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else { throw Failure(message: "the bridge is not running") }

        var line = try JSONSerialization.data(withJSONObject: body)
        line.append(UInt8(ascii: "\n"))
        let sent = line.withUnsafeBytes { write(fd, $0.baseAddress, line.count) }
        guard sent == line.count else { throw Failure(message: "could not send") }

        var reply = Data()
        var chunk = [UInt8](repeating: 0, count: 64 * 1024)
        while !reply.contains(UInt8(ascii: "\n")) {
            let count = read(fd, &chunk, chunk.count)
            guard count > 0 else { break }
            reply.append(contentsOf: chunk[0..<count])
        }
        if let object = try? JSONSerialization.jsonObject(with: reply) as? [String: Any],
           let message = object["error"] as? String
        {
            throw Failure(message: message)
        }
        return reply
    }
}
