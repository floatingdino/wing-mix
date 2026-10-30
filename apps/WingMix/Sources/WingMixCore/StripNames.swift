import Foundation

/// One named strip (`strips` reply).
public struct StripEntry: Decodable, Sendable, Equatable, Hashable {
    public var id: String
    public var kind: String?
    public var name: String
}

public struct StripList: Decodable, Sendable, Equatable {
    public var strips: [StripEntry]
}

/// Maps strip ids (`input.13`) to console names for display: `VOX 1 (ch 13)`. Unnamed or unknown strips read
/// `ch 13`. `humanize` rewrites every id inside free text (explanations, move labels, `input.13/fader` params).
public struct StripNames: Sendable, Equatable {
    public private(set) var names: [String: String] = [:]

    public init() {}

    public init(_ entries: [StripEntry]) {
        for e in entries where !e.name.trimmingCharacters(in: .whitespaces).isEmpty {
            names[e.id] = e.name.trimmingCharacters(in: .whitespaces)
        }
    }

    public var isEmpty: Bool { names.isEmpty }

    /// `input.13` -> `ch 13`, `bus.5` -> `bus 5`, ...
    public static func short(_ id: String) -> String {
        guard let dot = id.firstIndex(of: "."), let n = Int(id[id.index(after: dot)...]) else { return id }
        switch id[..<dot] {
        case "input": return "ch \(n)"
        case "aux": return "aux \(n)"
        case "bus": return "bus \(n)"
        case "main": return "main \(n)"
        case "matrix": return "mtx \(n)"
        case "dca": return "DCA \(n)"
        case "fx": return "fx \(n)"
        case "fx_return": return "fx ret \(n)"
        default: return id
        }
    }

    /// `VOX 1 (ch 13)`; `ch 13` without a name.
    public func label(_ id: String) -> String {
        let s = Self.short(id)
        guard let n = names[id] else { return s }
        return "\(n) (\(s))"
    }

    private static let idPattern = try! NSRegularExpression(
        pattern: #"\b(fx_return|input|aux|bus|main|matrix|dca|fx)\.(\d+)(?:/([A-Za-z0-9_./]+))?"#)

    /// Replace every strip id in `text` with its label; `input.13/fader` becomes `VOX 1 (ch 13) fader`.
    public func humanize(_ text: String) -> String {
        let ns = text as NSString
        var out = ""
        var last = 0
        for m in Self.idPattern.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
            out += ns.substring(with: NSRange(location: last, length: m.range.location - last))
            out += label(ns.substring(with: m.range(at: 1)) + "." + ns.substring(with: m.range(at: 2)))
            if m.range(at: 3).location != NSNotFound { out += " " + ns.substring(with: m.range(at: 3)) }
            last = m.range.location + m.range.length
        }
        return out + ns.substring(from: last)
    }
}
