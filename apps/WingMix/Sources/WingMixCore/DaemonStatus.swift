import Foundation

/// `status.json` (and the `status` command): the parts the app shows. Absent parts are `nil`.
public struct DaemonStatus: Decodable, Sendable, Equatable {
    public struct Console: Decodable, Sendable, Equatable {
        public var kind: String
        public var name: String?
        public var key: String?
        public var scene: String?
    }
    public struct Input: Decodable, Sendable, Equatable {
        public var kind: String
        public var description: String?
        public var sampleRate: Double?
        public var taps: [String]?
        public var droppedFrames: Int?
        public var clockS: Double?
        public var streamErrors: Int?
    }
    public struct Target: Decodable, Sendable, Equatable {
        public var kind: String
        public var label: String?
    }
    public struct Loading: Decodable, Sendable, Equatable {
        public var what: String?
        public var stage: String?
        public var progress: Double?
    }
    public struct Session: Decodable, Sendable, Equatable {
        public var running: Bool?
        public var stopped: Bool?
        public var holding: String?
        public var objective: Double?
        public var distance: Double?
        public var batches: Int?
        public var pending: Int?
        public var touched: Int?
    }
    public struct SafetyStatus: Decodable, Sendable, Equatable {
        public var feedbackLatched = false
        public var feedbackSuspect = false
        public var ceilingHold = false
        public var stalled = false
    }
    public struct Safety: Decodable, Sendable, Equatable {
        public var status: SafetyStatus?
        public var trips: Int?
    }

    public var running: Bool
    public var pid: Int
    public var version: String?
    public var updatedUnix: Double
    public var uptimeS: Double?
    public var stateDir: String?
    public var console: Console?
    public var input: Input?
    public var target: Target?
    public var loading: Loading?
    public var mode: String?
    public var session: Session?
    public var safety: Safety?
    public var lastError: String?

    public static func decode(_ data: Data) throws -> DaemonStatus {
        try Wire.decoder.decode(DaemonStatus.self, from: data)
    }

    /// The daemon is alive: it says so, its document is fresh and the process exists.
    public func isLive(now: Date = Date(), maxAge: TimeInterval = 5,
                       processExists: (Int) -> Bool = { kill(pid_t($0), 0) == 0 }) -> Bool {
        running && now.timeIntervalSince1970 - updatedUnix < maxAge && processExists(pid)
    }
}

/// Shared JSON decoding for replies and status.
public enum Wire {
    public static var decoder: JSONDecoder {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }
}

/// Reads `status.json`.
public enum StatusFile {
    public static func read(paths: DaemonPaths) -> DaemonStatus? {
        guard let data = try? Data(contentsOf: paths.statusURL) else { return nil }
        return try? DaemonStatus.decode(data)
    }
}
