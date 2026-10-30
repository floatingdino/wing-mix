import Foundation

/// Errors from the control socket.
public enum ControlError: Error, Equatable, Sendable, CustomStringConvertible {
    /// Socket path too long for `sockaddr_un`.
    case pathTooLong(String)
    /// connect/read/write failed (errno text).
    case io(String)
    /// The daemon answered `{"ok":false,"error":...}`.
    case daemon(String)
    /// The reply was not a protocol reply, or its result had an unexpected shape.
    case badReply(String)

    public var description: String {
        switch self {
        case .pathTooLong(let p): "control socket path too long: \(p)"
        case .io(let m): "control socket: \(m)"
        case .daemon(let m): m
        case .badReply(let r): "unexpected reply from wing-mixd: \(r)"
        }
    }
}

/// Client for wing-mixd's `control.sock` (docs/control-protocol.md): one JSON line per request,
/// one JSON line per reply. Each call uses its own short connection.
public struct ControlClient: Sendable {
    public let socketPath: String
    public var timeout: TimeInterval

    public init(socketURL: URL, timeout: TimeInterval = 30) {
        socketPath = socketURL.path(percentEncoded: false)
        self.timeout = timeout
    }

    /// `{"cmd":cmd, ...args}` as one line.
    public static func requestLine(_ cmd: String, _ args: [String: JSONValue] = [:]) -> String {
        var o = args
        o["cmd"] = .string(cmd)
        let data = (try? JSONEncoder.sortedKeys.encode(JSONValue.object(o))) ?? Data()
        return String(decoding: data, as: UTF8.self)
    }

    /// Send one request line and return the raw reply line; blocking (use `request` from async code).
    public func sendLine(_ line: String, timeout: TimeInterval? = nil) throws(ControlError) -> String {
        let timeout = timeout ?? self.timeout
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw .io(Self.errnoText("socket")) }
        defer { close(fd) }
        var one: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
        var tv = timeval(tv_sec: Int(timeout), tv_usec: Int32((timeout - timeout.rounded(.down)) * 1e6))
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(socketPath.utf8)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        guard pathBytes.count < capacity else { throw .pathTooLong(socketPath) }
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            raw.copyBytes(from: pathBytes)
            raw[pathBytes.count] = 0
        }
        addr.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard rc == 0 else { throw .io(Self.errnoText("connect \(socketPath)")) }

        let request = Array((line.trimmingCharacters(in: .whitespacesAndNewlines) + "\n").utf8)
        var sent = 0
        while sent < request.count {
            let n = request[sent...].withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
            guard n > 0 else { throw .io(Self.errnoText("write")) }
            sent += n
        }
        var reply: [UInt8] = []
        var buf = [UInt8](repeating: 0, count: 16_384)
        while !reply.contains(UInt8(ascii: "\n")) {
            let n = buf.withUnsafeMutableBytes { read(fd, $0.baseAddress, $0.count) }
            if n < 0 { throw .io(Self.errnoText("read")) }
            if n == 0 { break }
            reply.append(contentsOf: buf[..<n])
        }
        return String(decoding: reply, as: UTF8.self)
    }

    /// Run `cmd` and return its `result`, off the main actor.
    @concurrent
    public func request(_ cmd: String, _ args: [String: JSONValue] = [:],
                        timeout: TimeInterval? = nil) async throws(ControlError) -> JSONValue {
        try Self.parseReply(try sendLine(Self.requestLine(cmd, args), timeout: timeout))
    }

    /// `request` decoded into `T` (snake_case keys).
    @concurrent
    public func call<T: Decodable & Sendable>(_ type: T.Type, _ cmd: String, _ args: [String: JSONValue] = [:],
                                              timeout: TimeInterval? = nil) async throws(ControlError) -> T {
        let raw = try sendLine(Self.requestLine(cmd, args), timeout: timeout)
        return try Self.parseReply(raw, as: type)
    }

    /// `{"ok":true,"result":x}` -> `x`; `{"ok":false,"error":m}` -> `.daemon(m)`.
    public static func parseReply(_ raw: String) throws(ControlError) -> JSONValue {
        let line = raw.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            .first.map(String.init) ?? ""
        guard !line.isEmpty else { throw .badReply("(no reply)") }
        guard let v = try? JSONDecoder().decode(JSONValue.self, from: Data(line.utf8)),
              let ok = v["ok"]?.bool
        else { throw .badReply(String(line.prefix(200))) }
        if !ok { throw .daemon(v["error"]?.string ?? "error") }
        return v["result"] ?? .null
    }

    /// As `parseReply`, decoding the result into `T`.
    public static func parseReply<T: Decodable>(_ raw: String, as type: T.Type) throws(ControlError) -> T {
        let line = raw.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            .first.map(String.init) ?? ""
        _ = try parseReply(raw)
        do {
            return try Wire.decoder.decode(Envelope<T>.self, from: Data(line.utf8)).result
        } catch {
            throw .badReply("\(error)")
        }
    }

    private static func errnoText(_ what: String) -> String {
        "\(what): \(String(cString: strerror(errno)))"
    }
}

private struct Envelope<R: Decodable>: Decodable { var result: R }

extension JSONEncoder {
    static var sortedKeys: JSONEncoder {
        let e = JSONEncoder()
        e.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return e
    }
}
