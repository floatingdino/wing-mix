import Foundation

/// Mix slots (M064): `slot_list`, `slot_recall`, `slot_diff` replies and the slot strip's UI-free logic.
public struct SlotList: Decodable, Sendable, Equatable {
    public var slots: [SlotSummary] = []
    public var active: String?
    public var comparing: Bool = false
    public var switchingSet: Int?
    public var liveConfirmationNeeded: Bool = false
    public var target: SlotTargetRef?
    public var distance: Double?
    public var objective: Double?

    public init(slots: [SlotSummary] = [], active: String? = nil, comparing: Bool = false) {
        self.slots = slots
        self.active = active
        self.comparing = comparing
    }

    enum CodingKeys: String, CodingKey {
        case slots, active, comparing, switchingSet, liveConfirmationNeeded, target, distance, objective
    }

    public init(from d: Decoder) throws {
        let c = try d.container(keyedBy: CodingKeys.self)
        slots = try c.decodeIfPresent([SlotSummary].self, forKey: .slots) ?? []
        active = try c.decodeIfPresent(String.self, forKey: .active)
        comparing = try c.decodeIfPresent(Bool.self, forKey: .comparing) ?? false
        switchingSet = try c.decodeIfPresent(Int.self, forKey: .switchingSet)
        liveConfirmationNeeded = try c.decodeIfPresent(Bool.self, forKey: .liveConfirmationNeeded) ?? false
        target = try c.decodeIfPresent(SlotTargetRef.self, forKey: .target)
        distance = try c.decodeIfPresent(Double.self, forKey: .distance)
        objective = try c.decodeIfPresent(Double.self, forKey: .objective)
    }

    /// Slot for hotkey `n` (1-based, in display order: `Original` first).
    public func slot(forHotkey n: Int) -> SlotSummary? {
        (1...9).contains(n) && n <= slots.count ? slots[n - 1] : nil
    }

    /// Hotkey of a slot (1-9), if it has one.
    public func hotkey(of name: String) -> Int? {
        guard let i = slots.firstIndex(where: { $0.name == name }), i < 9 else { return nil }
        return i + 1
    }
}

public struct SlotTargetRef: Decodable, Sendable, Equatable {
    public var kind: String?
    public var label: String?
    public var liveContext: String?
}

public struct SlotSummary: Decodable, Sendable, Equatable, Identifiable {
    public var name: String
    public var builtin: Bool?
    public var auto: Bool?
    public var storedUnix: Double?
    public var target: SlotTargetRef?
    public var distance: Double?
    public var objective: Double?
    public var params: Int?
    public var differs: Int?
    public var engineerMoved: Int?
    public var active: Bool?
    public var compatible: Bool?
    public var refused: String?
    public var id: String { name }

    public init(name: String, target: String? = nil, distance: Double? = nil, differs: Int = 0, active: Bool = false) {
        self.name = name
        self.target = target.map { SlotTargetRef(kind: "reference", label: $0, liveContext: "live") }
        self.distance = distance
        self.differs = differs
        self.active = active
        compatible = true
    }

    /// Second line of a slot button: what it was made for.
    public var targetLine: String {
        if builtin == true { return "session start" }
        return target?.label.map { URL(filePath: $0).lastPathComponent } ?? "no target"
    }

    /// Third line: distance when stored and how much differs from the console now.
    public var detailLine: String {
        var parts: [String] = []
        if let d = distance { parts.append(String(format: "dist %.2f", d)) }
        if let n = differs { parts.append(n == 0 ? "= console" : "\(n) differ") }
        if let m = engineerMoved, m > 0 { parts.append("\(m) moved by you") }
        return parts.joined(separator: " · ")
    }
}

/// `slot_recall` reply.
public struct SlotRecall: Decodable, Sendable, Equatable {
    public var name: String
    public var written: Int?
    public var skipped: [JSONValue]?
    public var keptEngineer: [String]?
    public var models: [String]?
    public var mayClick: Bool?
    public var phases: Int?
    public var writeMs: Double?
    public var requestMs: Double?

    /// One line for the strip: "B: 9 params in 1 batch (0.1 ms), kept 2 of your moves".
    public var summary: String {
        var s = "\(name): \(written ?? 0) params in \(phases ?? 0) batch\((phases ?? 0) == 1 ? "" : "es")"
        if let r = requestMs { s += String(format: " (%.0f ms)", r) }
        if let m = models, !m.isEmpty { s += ", model switch first" }
        if let k = keptEngineer, !k.isEmpty { s += ", kept \(k.count) of your moves" }
        if let sk = skipped, !sk.isEmpty { s += ", \(sk.count) not allowed" }
        return s
    }
}

/// `slot_diff` reply.
public struct SlotDiff: Decodable, Sendable, Equatable {
    public var a: String
    public var b: String
    public var params: [Row] = []

    public struct Row: Decodable, Sendable, Equatable, Identifiable {
        public var param: String
        public var a: ParamValue?
        public var b: ParamValue?
        public var engineerMoved: Bool?
        public var id: String { param }
    }

    /// Rows grouped by strip id (`input.13/eq/band.1.gain` -> `input.13`), strips in order of first appearance.
    public var byStrip: [(strip: String, rows: [Row])] {
        var order: [String] = []
        var groups: [String: [Row]] = [:]
        for r in params {
            let strip = String(r.param.split(separator: "/").first ?? Substring(r.param))
            if groups[strip] == nil { order.append(strip) }
            groups[strip, default: []].append(r)
        }
        return order.map { ($0, groups[$0] ?? []) }
    }
}
