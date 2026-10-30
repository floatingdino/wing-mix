import Foundation

/// Bulk role editing for the Remit screen (M065): selections (kind, numeric range, name match, needs-confirmation),
/// the change list a bulk action produces (shown for confirmation before anything is sent), the `confirm_roles`
/// batch that applies it, and the batch that undoes it (from the reply's `previous`).
public enum RoleBulk {
    /// Roles the remit protects (writes refused): changing a strip *away* from these needs care.
    public static let protective: Set<String> = ["monitor", "comms", "aux_feed", "unknown", "unrouted", "none"]

    /// One strip's change. `to == nil` clears the confirmation (back to detection).
    public struct Change: Sendable, Equatable, Identifiable {
        public var strip: String
        public var name: String
        /// Role in effect now.
        public var from: String
        /// New confirmed role, or nil = clear the confirmation.
        public var to: String?
        /// Role in effect after the change (`to`, or the detection's effective role when cleared).
        public var after: String
        public var id: String { strip }

        /// The change lifts protection (e.g. monitor -> fx): listed prominently in the confirmation.
        public var unprotects: Bool { RoleBulk.protective.contains(from) && !RoleBulk.protective.contains(after) }
    }

    // MARK: selection

    /// `bus.12` -> ("bus", 12).
    public static func parts(_ strip: String) -> (kind: String, index: Int?) {
        let p = strip.split(separator: ".", maxSplits: 1)
        return (String(p.first ?? ""), p.count > 1 ? Int(p[1]) : nil)
    }

    /// Console order: inputs, aux, buses, mains, matrices, then the rest; numbers ascending (`bus.2` before `bus.10`).
    public static func consoleOrder(_ a: String, _ b: String) -> Bool {
        let kinds = ["input", "aux", "bus", "main", "matrix", "dca", "fx"]
        let (pa, pb) = (parts(a), parts(b))
        let (ka, kb) = (kinds.firstIndex(of: pa.kind) ?? kinds.count, kinds.firstIndex(of: pb.kind) ?? kinds.count)
        if ka != kb { return ka < kb }
        if pa.kind != pb.kind { return pa.kind < pb.kind }
        return (pa.index ?? 0) < (pb.index ?? 0)
    }

    public static func ofKind(_ strips: [RoleStrip], _ kind: String) -> Set<String> {
        Set(strips.filter { parts($0.strip).kind == kind }.map(\.strip))
    }

    /// Strips of `kind` numbered within `range` ("buses 8-16").
    public static func inRange(_ strips: [RoleStrip], kind: String, _ range: ClosedRange<Int>) -> Set<String> {
        Set(strips.filter {
            let p = parts($0.strip)
            return p.kind == kind && p.index.map(range.contains) == true
        }.map(\.strip))
    }

    /// Strips whose name contains `text` (case-insensitive, whole text; empty matches nothing).
    public static func nameContains(_ strips: [RoleStrip], _ text: String) -> Set<String> {
        let t = text.trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty else { return [] }
        return Set(strips.filter { $0.name.range(of: t, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
            .map(\.strip))
    }

    public static func needingConfirmation(_ strips: [RoleStrip]) -> Set<String> {
        Set(strips.filter(\.needsConfirmation).map(\.strip))
    }

    /// Shift-click: every strip between `anchor` and `target` in display order (inclusive).
    public static func span(_ order: [String], from anchor: String, to target: String) -> Set<String> {
        guard let a = order.firstIndex(of: anchor), let b = order.firstIndex(of: target) else { return [target] }
        return Set(order[min(a, b)...max(a, b)])
    }

    // MARK: changes

    /// Role in effect now (confirmed, else detection's effective role).
    public static func current(_ s: RoleStrip) -> String { s.confirmed ?? s.effective }

    private static func change(_ s: RoleStrip, to: String?) -> Change? {
        let after = to ?? s.effective
        // no-op: same confirmation already stored (or nothing to clear)
        if to == nil, s.confirmed == nil { return nil }
        if let t = to, s.confirmed == t { return nil }
        return Change(strip: s.strip, name: s.name, from: current(s), to: to, after: after)
    }

    private static func pick(_ strips: [RoleStrip], _ selection: Set<String>) -> [RoleStrip] {
        strips.filter { selection.contains($0.strip) }.sorted { consoleOrder($0.strip, $1.strip) }
    }

    /// Confirm `role` for every selected strip.
    public static func setRole(_ role: String, _ strips: [RoleStrip], _ selection: Set<String>) -> [Change] {
        pick(strips, selection).compactMap { change($0, to: role) }
    }

    /// Lock: confirm `unknown` (off limits) for every selected strip.
    public static func lock(_ strips: [RoleStrip], _ selection: Set<String>) -> [Change] {
        setRole("unknown", strips, selection)
    }

    /// Unlock: confirm the detected role of every selected strip whose detection names a role (`unknown` detections
    /// are skipped: there is nothing to unlock to without choosing a role).
    public static func unlock(_ strips: [RoleStrip], _ selection: Set<String>) -> [Change] {
        pick(strips, selection).filter { $0.detected != "unknown" }.compactMap { change($0, to: $0.detected) }
    }

    /// Back to detection: clear every confirmation in the selection.
    public static func reset(_ strips: [RoleStrip], _ selection: Set<String>) -> [Change] {
        pick(strips, selection).compactMap { change($0, to: nil) }
    }

    /// Accept the detected role of every unconfirmed strip needing confirmation whose confidence is at least
    /// `minConfidence` (0...1), within `selection` when one is given.
    public static func acceptSuggestions(_ strips: [RoleStrip], minConfidence: Double,
                                         within selection: Set<String>? = nil) -> [Change] {
        strips.sorted { consoleOrder($0.strip, $1.strip) }.filter {
            $0.needsConfirmation && $0.confirmed == nil && $0.confidence >= minConfidence
                && $0.detected != "unknown" && (selection?.contains($0.strip) ?? true)
        }
        .compactMap { change($0, to: $0.detected) }
    }

    // MARK: wire

    /// The `confirm_roles` `roles` object of a change list (null = clear).
    public static func batch(_ changes: [Change]) -> JSONValue {
        .object(Dictionary(uniqueKeysWithValues: changes.map { ($0.strip, $0.to.map(JSONValue.string) ?? .null) }))
    }

    /// The undo batch from a `confirm_roles` reply (`previous`: strip -> role or null).
    public static func undoBatch(fromReply reply: JSONValue?) -> JSONValue? {
        guard case .object(let prev)? = reply?["previous"], !prev.isEmpty else { return nil }
        return .object(prev)
    }
}
