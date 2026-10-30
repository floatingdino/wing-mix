import Foundation

/// Operating modes of the controller.
public enum MixMode: String, CaseIterable, Sendable, Identifiable {
    case observe, suggest, autopilot
    public var id: String { rawValue }
}

/// The live-context target setting (`set_live_context`, M037).
public enum LiveContext: String, CaseIterable, Sendable, Identifiable {
    case live, broadcastVideo = "broadcast_video", matchRecord = "match_record"
    public var id: String { rawValue }
    public var title: String {
        switch self {
        case .live: "Live room (PA for an audience)"
        case .broadcastVideo: "Broadcast / video"
        case .matchRecord: "Match the recording"
        }
    }
}

/// `{"type":"float","value":-4.0}`.
public struct ParamValue: Decodable, Sendable, Equatable {
    public var type: String?
    public var value: JSONValue?
    public var text: String {
        guard let v = value else { return "-" }
        if let d = v.double { return String(format: "%.1f", d) }
        return v.display
    }
}

public struct ParamChange: Decodable, Sendable, Equatable {
    public var param: String
    public var from: ParamValue?
    public var to: ParamValue?
    public var text: String { "\(param): \(from?.text ?? "?") -> \(to?.text ?? "?")" }
}

public struct Loudness: Decodable, Sendable, Equatable {
    public var momentaryLufs: Double?
    public var shortTermLufs: Double?
    public var truePeakDbtp: Double?
    public var crestDb: Double?
    public var tiltDbPerOct: Double?
    public var widthDb: Double?
    public var correlation: Double?
}

public struct Spectrum: Decodable, Sendable, Equatable {
    public var erbHz: [Double] = []
    public var currentErbRelDb: [Double] = []
    public var targetErbRelDb: [Double] = []
    public var targetErbLoDb: [Double] = []
    public var targetErbHiDb: [Double] = []
    public var toleranceDb: [Double] = []

    /// One chart row per ERB band (frequency, current, target, p10, p90); empty when the arrays disagree.
    public var points: [Point] {
        let n = erbHz.count
        guard n > 0, currentErbRelDb.count == n, targetErbRelDb.count == n else { return [] }
        return (0..<n).map { i in
            Point(hz: erbHz[i], current: currentErbRelDb[i], target: targetErbRelDb[i],
                  lo: targetErbLoDb.count == n ? targetErbLoDb[i] : targetErbRelDb[i],
                  hi: targetErbHiDb.count == n ? targetErbHiDb[i] : targetErbRelDb[i])
        }
    }

    public struct Point: Sendable, Equatable, Identifiable {
        public var hz: Double, current: Double, target: Double, lo: Double, hi: Double
        public var id: Double { hz }
    }
}

public struct PlanScore: Decodable, Sendable, Equatable {
    public var objective: Double?
    public var distance: Double?
    public var problems: Int?
    public var targetLoad: Double?
}

public struct CurvePoint: Decodable, Sendable, Equatable, Identifiable {
    public var atS: Double
    public var kept: Int?
    public var score: PlanScore?
    public var id: Double { atS }
}

public struct Contributor: Decodable, Sendable, Equatable {
    public var strip: String
    public var share: Double?
}

public struct Problem: Decodable, Sendable, Equatable, Identifiable {
    public var id: String
    public var kind: String?
    public var severity: Double?
    public var confidence: Double?
    public var explanation: String?
    public var contributors: [Contributor]?
    public var suggestedBlocks: [String]?
}

/// A planned / suggested move (`MoveSummary`).
public struct MoveSummary: Decodable, Sendable, Equatable, Identifiable {
    public var label: String
    public var strip: String?
    public var phase: String?
    public var problem: String?
    public var explanation: String?
    public var changes: [ParamChange]?
    public var predictedGain: Double?
    public var needsConfirmation: [String]?
    public var inspect: Bool?
    public var id: String { "\(strip ?? "")|\(label)" }
}

public struct PendingMove: Decodable, Sendable, Equatable, Identifiable {
    public var key: String
    public var summary: MoveSummary?
    public var id: String { key + "|" + (summary?.id ?? "") }
}

/// One batch the controller ran (`moves`).
public struct MoveRecord: Decodable, Sendable, Equatable, Identifiable {
    public var seq: Int
    public var atS: Double?
    public var summary: MoveSummary?
    public var outcome: String?
    public var before: Double?
    public var after: Double?
    public var note: String?
    public var id: Int { seq }
}

public struct SafetyTrip: Decodable, Sendable, Equatable {
    public var atS: Double?
    public var trip: JSONValue?
    public var reverted: JSONValue?
}

/// The console RTA the feedback guard listens to (`safety.rta_feed`; WING sessions only).
public struct RtaFeed: Decodable, Sendable, Equatable {
    public var frames: Int?
    public var source: String?
}

public struct SnapshotSafety: Decodable, Sendable, Equatable {
    public var status: DaemonStatus.SafetyStatus?
    public var trips: [SafetyTrip] = []
    public var rtaFeed: RtaFeed?
}

public struct SnapshotSession: Decodable, Sendable, Equatable {
    public var running: Bool?
    public var stopped: Bool?
    public var feedbackGuard: Bool?
    public var holding: String?
    public var busy: Bool?
    public var budget: [Double]?
}

public struct TouchedParam: Decodable, Sendable, Equatable, Identifiable {
    public var param: String
    public var baseline: ParamValue?
    public var current: ParamValue?
    public var id: String { param }
}

/// The `snapshot` reply: everything the Live screen shows.
public struct Snapshot: Decodable, Sendable, Equatable {
    public var atS: Double?
    public var mode: String?
    public var liveContext: String?
    /// A target is loaded, so the live-context transform is active (otherwise only stored).
    public var liveContextApplied: Bool?
    public var console: DaemonStatus.Console?
    public var target: SnapshotTarget?
    public var loudness: Loudness?
    public var spectrum: Spectrum?
    public var session: SnapshotSession?
    public var score: PlanScore?
    public var curve: [CurvePoint]
    public var problems: [Problem]
    public var diagnosisNotes: [String]
    public var plan: [MoveSummary]
    public var pending: [PendingMove]
    public var moves: [MoveRecord]
    public var log: [JSONValue]
    public var safety: SnapshotSafety?
    public var touched: [TouchedParam]

    enum CodingKeys: String, CodingKey {
        case atS, mode, liveContext, liveContextApplied, console, target, loudness, spectrum, session, score, curve, problems, diagnosisNotes, plan,
             pending, moves, log, safety, touched
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        atS = try c.decodeIfPresent(Double.self, forKey: .atS)
        mode = try c.decodeIfPresent(String.self, forKey: .mode)
        liveContext = try c.decodeIfPresent(String.self, forKey: .liveContext)
        liveContextApplied = try c.decodeIfPresent(Bool.self, forKey: .liveContextApplied)
        console = try c.decodeIfPresent(DaemonStatus.Console.self, forKey: .console)
        target = try c.decodeIfPresent(SnapshotTarget.self, forKey: .target)
        loudness = try c.decodeIfPresent(Loudness.self, forKey: .loudness)
        spectrum = try c.decodeIfPresent(Spectrum.self, forKey: .spectrum)
        session = try c.decodeIfPresent(SnapshotSession.self, forKey: .session)
        score = try c.decodeIfPresent(PlanScore.self, forKey: .score)
        curve = try c.decodeIfPresent([CurvePoint].self, forKey: .curve) ?? []
        problems = try c.decodeIfPresent([Problem].self, forKey: .problems) ?? []
        diagnosisNotes = try c.decodeIfPresent([String].self, forKey: .diagnosisNotes) ?? []
        plan = try c.decodeIfPresent([MoveSummary].self, forKey: .plan) ?? []
        pending = try c.decodeIfPresent([PendingMove].self, forKey: .pending) ?? []
        moves = try c.decodeIfPresent([MoveRecord].self, forKey: .moves) ?? []
        log = try c.decodeIfPresent([JSONValue].self, forKey: .log) ?? []
        safety = try c.decodeIfPresent(SnapshotSafety.self, forKey: .safety)
        touched = try c.decodeIfPresent([TouchedParam].self, forKey: .touched) ?? []
    }
}

public struct SnapshotTarget: Decodable, Sendable, Equatable {
    public var kind: String
    public var label: String?
    public var info: JSONValue?
}

// MARK: - Setup replies

public struct AudioDevice: Decodable, Sendable, Equatable, Identifiable, Hashable {
    public var name: String
    public var `default`: Bool
    public var maxChannels: Int
    public var defaultSampleRate: Double?
    public var id: String { name }
}

public struct DeviceList: Decodable, Sendable, Equatable {
    public var devices: [AudioDevice]
    public var sim: SimInfo?
    public var error: String?
    public struct SimInfo: Decodable, Sendable, Equatable { public var taps: [String] }
}

public struct ConsoleEntry: Decodable, Sendable, Equatable, Identifiable, Hashable {
    public var kind: String
    public var ip: String?
    public var name: String?
    public var model: String?
    public var serial: String?
    public var firmware: String?
    public var id: String { kind == "sim" ? "sim" : (ip ?? name ?? kind) }
    public var title: String {
        kind == "sim" ? "Embedded simulator (wing-sim)" : "\(name ?? "WING") \(model.map { "(\($0))" } ?? "")"
    }
}

public struct ConsoleList: Decodable, Sendable, Equatable {
    public var consoles: [ConsoleEntry]
}

/// Which console signal a group of input channels carries (`select_input` `taps`).
public struct TapSpec: Sendable, Equatable, Identifiable {
    public var id = UUID()
    /// `main`, `solo`, `input.N`, `monitor:NAME`.
    public var tap: String
    /// First (1-based) channel.
    public var channel: Int
    public var stereo: Bool

    public init(tap: String, channel: Int, stereo: Bool) {
        self.tap = tap
        self.channel = channel
        self.stereo = stereo
    }

    public var json: JSONValue {
        let ch: [JSONValue] = stereo ? [.number(Double(channel)), .number(Double(channel + 1))] : [.number(Double(channel))]
        return ["tap": .string(tap), "channels": .array(ch)]
    }
    public static let defaultMain = TapSpec(tap: "main", channel: 1, stereo: true)
}

// MARK: - Remit

public enum StripRole {
    /// Roles `confirm_roles` accepts.
    public static let all = ["foh", "monitor", "comms", "unrouted", "foh_group", "fx", "aux_feed", "unknown", "main",
                             "matrix", "dca", "none"]
}

public struct RoleStrip: Decodable, Sendable, Equatable, Identifiable {
    public var strip: String
    public var name: String
    public var detected: String
    public var effective: String
    public var confidence: Double
    public var evidence: [String]
    public var confirmed: String?
    public var needsConfirmation: Bool
    public var id: String { strip }
    /// What the Remit screen offers: the confirmed role, else the detected one.
    public var proposed: String { confirmed ?? detected }
}

public struct RoleList: Decodable, Sendable, Equatable {
    public var key: String
    public var strips: [RoleStrip]
}
