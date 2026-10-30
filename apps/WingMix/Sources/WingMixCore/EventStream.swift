import Foundation

/// One line of the daemon's event stream (`subscribe`): `{"source","type","at_s","unix",...}`.
public struct DaemonEvent: Sendable, Equatable {
    public var source: String
    public var type: String
    public var atS: Double?
    public var raw: JSONValue

    public static func parse(_ line: String) -> DaemonEvent? {
        guard let v = try? JSONDecoder().decode(JSONValue.self, from: Data(line.utf8)),
              let source = v["source"]?.string, let type = v["type"]?.string
        else { return nil }
        return DaemonEvent(source: source, type: type, atS: v["at_s"]?.double, raw: v)
    }

    /// Events after which the strip names may have changed.
    public var changesStrips: Bool {
        source == "daemon" && ["console_connected", "console_disconnected", "roles_confirmed"].contains(type)
    }
}

/// What an `EventStream` yields.
public enum StreamItem: Sendable, Equatable {
    case connected
    case event(DaemonEvent)
    /// The connection ended (the stream reconnects by itself); the text says why.
    case disconnected(String)
}

/// Splits a byte stream into lines.
public struct LineBuffer: Sendable {
    private var pending: [UInt8] = []
    public init() {}

    public mutating func append(_ bytes: some Sequence<UInt8>) -> [String] {
        pending.append(contentsOf: bytes)
        var lines: [String] = []
        while let nl = pending.firstIndex(of: UInt8(ascii: "\n")) {
            lines.append(String(decoding: pending[..<nl], as: UTF8.self))
            pending.removeSubrange(...nl)
        }
        return lines
    }
}

/// Subscribes to wing-mixd's event stream and reconnects with a back-off when the daemon goes away or goes
/// silent (it sends a heartbeat every 2 s, so `silenceLimit` of quiet means a dead connection).
public struct EventStream: Sendable {
    public let socketPath: String
    public var silenceLimit: TimeInterval
    public var maxBackoff: TimeInterval

    public init(socketURL: URL, silenceLimit: TimeInterval = 7, maxBackoff: TimeInterval = 4) {
        socketPath = socketURL.path(percentEncoded: false)
        self.silenceLimit = silenceLimit
        self.maxBackoff = maxBackoff
    }

    /// Items until the consuming task is cancelled.
    public func items() -> AsyncStream<StreamItem> {
        AsyncStream { continuation in
            let task = Task { await run(continuation) }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    @concurrent
    private func run(_ out: AsyncStream<StreamItem>.Continuation) async {
        var backoff = 0.5
        while !Task.isCancelled {
            let why = connectAndRead(out, resetBackoff: { backoff = 0.5 })
            guard !Task.isCancelled else { break }
            out.yield(.disconnected(why))
            try? await Task.sleep(for: .seconds(backoff))
            backoff = min(maxBackoff, backoff * 2)
        }
        out.finish()
    }

    /// Blocking: one connection's lifetime. Returns the reason it ended.
    private func connectAndRead(_ out: AsyncStream<StreamItem>.Continuation, resetBackoff: () -> Void) -> String {
        let fd: Int32
        do { fd = try ControlClient.openSocket(path: socketPath, timeout: 1) } catch { return "\(error)" }
        defer { close(fd) }
        let hello = Array("{\"cmd\":\"subscribe\"}\n".utf8)
        guard write(fd, hello, hello.count) == hello.count else { return "write failed" }

        var lines = LineBuffer()
        var acknowledged = false
        var lastData = Date()
        var buf = [UInt8](repeating: 0, count: 16_384)
        while !Task.isCancelled {
            let n = buf.withUnsafeMutableBytes { read(fd, $0.baseAddress, $0.count) }
            if n == 0 { return "closed by the daemon" }
            if n < 0 {
                if errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR {
                    if Date().timeIntervalSince(lastData) > silenceLimit { return "no heartbeat" }
                    continue
                }
                return ControlClient.errnoText("read")
            }
            lastData = Date()
            for line in lines.append(buf[..<n]) {
                if !acknowledged {
                    guard (try? ControlClient.parseReply(line)) != nil else { return "subscribe refused: \(line.prefix(120))" }
                    acknowledged = true
                    resetBackoff()
                    out.yield(.connected)
                } else if let e = DaemonEvent.parse(line) {
                    out.yield(.event(e))
                }
            }
        }
        return "cancelled"
    }
}
