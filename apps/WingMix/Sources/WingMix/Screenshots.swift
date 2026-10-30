import AppKit
import SwiftUI
import WingMixCore

/// `WINGMIX_SHOTS=<dir>` (with `WINGMIX_DEMO`): after the demo session has produced data, render each screen of the
/// main window into `<dir>/<screen>.png`, then the autopilot states of the Live screen, and quit. Used for docs/user/
/// because `screencapture` needs screen-recording permission that headless runs do not have.
///
/// Autopilot states: `live-autopilot.png` is a real autopilot run (moves made, kept/reverted). `live-confirmation.png`
/// and `live-safety.png` take the real snapshot and add a pending confirmation / a feedback trip with an RTA feed
/// (the sim does not produce these on demand); `live-stopped.png` is a real STOP.
extension AppModel {
    func captureScreens(to dir: String) async {
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try? await Task.sleep(for: .seconds(25))  // let the session measure and plan
        for s in Screen.allCases {
            screen = s
            if s == .target { await refreshStatus() }
            try? await Task.sleep(for: .seconds(2))
            render(s, to: dir + "/" + s.rawValue.lowercased() + ".png")
        }
        await captureAutopilotStates(dir)
        NSApp.terminate(nil)
    }

    private func captureAutopilotStates(_ dir: String) async {
        screen = .live
        await setMode(.autopilot)
        // wait for some kept/reverted moves (up to ~3 min)
        for _ in 0..<90 where (snapshot?.moves.count ?? 0) < 3 {
            try? await Task.sleep(for: .seconds(2))
        }
        try? await Task.sleep(for: .seconds(1))
        render(.live, to: dir + "/live-autopilot.png")

        // pending confirmation (fixture: the plan's first move as a move needing a confirmation)
        if var snap = snapshot {
            let label = snap.plan.first?.label ?? snap.moves.last?.summary?.label ?? "input.13 fader +2.9 dB"
            let strip = snap.plan.first?.strip ?? "input.13"
            let json = #"{"key":"\#(strip)","summary":{"label":"\#(label)","strip":"\#(strip)","needs_confirmation":["\#(strip)"]}}"#
            if let p = try? Wire.decoder.decode(PendingMove.self, from: Data(json.utf8)) {
                snap.pending = [p]
                frozenSnapshot = snap
                snapshot = snap
                render(.live, to: dir + "/live-confirmation.png")
            }
        }
        frozenSnapshot = nil

        // real STOP, then a fixture feedback trip + RTA feed on top of the real snapshot
        await emergencyStop()
        try? await Task.sleep(for: .seconds(2))
        await refreshStatus()
        render(.live, to: dir + "/live-stopped.png")
        if var snap = snapshot {
            let json = """
            {"status":{"feedback_latched":true,"feedback_suspect":true,"ceiling_hold":false,"stalled":false},
             "trips":[{"at_s":\(snap.atS ?? 40),"trip":"feedback ring at 2.4 kHz (rta:input.13)","reverted":[]}],
             "rta_feed":{"frames":1843,"source":"rta:input.13"}}
            """
            if let sf = try? Wire.decoder.decode(SnapshotSafety.self, from: Data(json.utf8)) {
                snap.safety = sf
                frozenSnapshot = snap
                snapshot = snap
                render(.live, to: dir + "/live-safety.png")
            }
        }
        frozenSnapshot = nil
    }

    private func render(_ s: Screen, to path: String) {
        let r = ImageRenderer(content: ShotView(model: self, screen: s)
            .frame(width: 1120)
            .frame(minHeight: 780, alignment: .top)
            .fixedSize(horizontal: false, vertical: true)
            .background(Color(nsColor: .windowBackgroundColor))
            .environment(\.colorScheme, .light)
            .environment(\.shotMode, true))
        r.scale = 1.5
        if let img = r.nsImage, let tiff = img.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff),
           let png = rep.representation(using: .png, properties: [:])
        {
            try? png.write(to: URL(filePath: path))
        }
    }
}

/// The window's content without the AppKit-backed split view (ImageRenderer cannot draw it).
struct ShotView: View {
    var model: AppModel
    var screen: Screen

    var body: some View {
        VStack(spacing: 0) {
            ControlBar(model: model)
            Divider()
            switch screen {
            case .target: TargetView(model: model)
            case .listen: ListenView(model: model)
            case .remit: RemitView(model: model)
            case .live: LiveView(model: model)
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }
}
