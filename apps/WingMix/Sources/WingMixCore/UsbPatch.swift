import Foundation

/// What a WING (or wing-sim) sends to its USB outputs, i.e. what the "Wing Simulator" / WING audio device's *input*
/// channels carry. Read from the console's output patch (`output.usb.<n>/grp` + `/in`, generic ids of
/// `io/out/USB/<n>`), shown next to each Listen tap with a warning when the tap does not get the signal it claims.
public enum UsbPatch {
    /// Generic param ids to read for device channels `channels` (1-based = USB out number).
    public static func paramIds(_ channels: [Int]) -> [String] {
        channels.flatMap { ["output.usb.\($0)/grp", "output.usb.\($0)/in"] }
    }

    /// `(grp, in)` per USB out from a `params` reply (`{"output.usb.45/grp":{"type":"enum","value":"MTX"},...}`).
    public static func parse(_ reply: JSONValue?, channels: [Int]) -> [Int: (grp: String, input: Int)] {
        var out: [Int: (grp: String, input: Int)] = [:]
        for c in channels {
            guard let g = reply?["output.usb.\(c)/grp"]?["value"]?.string else { continue }
            let i = reply?["output.usb.\(c)/in"]?["value"]?.double.map { Int($0) } ?? 0
            out[c] = (g, i)
        }
        return out
    }

    /// Human label of an output source: mix outputs are mono legs (`MAIN` 1/2 = main 1 L/R, `MON` 1/2 = monitor A
    /// L/R, which carries the solo bus while something is soloed).
    public static func describe(grp: String, input: Int) -> String {
        let g = grp.uppercased()
        let leg = { (n: Int) in "\((n + 1) / 2) \(n % 2 == 1 ? "L" : "R")" }
        switch g {
        case "OFF", "": return "nothing (off)"
        case "MAIN": return "Main \(leg(input))"
        case "BUS": return "Bus \(leg(input))"
        case "MTX": return "Matrix \(leg(input))"
        case "MON": return input <= 2 ? "Monitor A \(input == 1 ? "L" : "R") (solo)" : "Monitor B \(input == 3 ? "L" : "R")"
        case "USR": return "User signal \(input)"
        default: return "\(g) \(input)"
        }
    }

    /// A warning when the patch of `channels` does not carry what `tap` says (nil = fine or unknown).
    /// `main`: MAIN legs 1/2 (main 1 L/R) in order; `solo`: MON legs 1/2 (monitor A, the solo bus).
    public static func warning(tap: String, channels: [Int], patch: [Int: (grp: String, input: Int)]) -> String? {
        let want: [(String, Int)]
        switch tap {
        case "main": want = [("MAIN", 1), ("MAIN", 2)]
        case "solo": want = [("MON", 1), ("MON", 2)]
        default: return nil
        }
        let got = channels.map { patch[$0] }
        if got.contains(where: { $0 == nil }) { return nil }
        for (k, c) in channels.enumerated() where k < want.count {
            let p = patch[c]!
            if p.grp.uppercased() != want[k].0 || p.input != want[k].1 {
                let what = tap == "main" ? "Main 1 L/R" : "the monitor/solo bus (MON 1/2)"
                return "USB out \(c) carries \(describe(grp: p.grp, input: p.input)), not \(what). Patch it in WING-Edit (Routing > USB outputs) or pick other channels."
            }
        }
        return nil
    }
}
