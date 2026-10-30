import AppKit
import SwiftUI
import WingMixCore

/// Wing Mix: front end for the wing-mixd daemon (M051).
///
/// `--smoke-test`: build the model (reads the status file, locates the daemon binary, starts nothing), print a
/// line and quit after a second; used by `smoke.sh` / `make app`.
/// `WINGMIX_DEMO=<dir>` + `WINGMIX_SCREEN=target|listen|remit|live`: demo session for the screenshots.
/// `WINGMIX_REMIT_SHOT=<scene.snap>` + `WINGMIX_SHOTS=<dir>`: Remit bulk-editing screenshots (Screenshots.swift).
@main
struct WingMixApp: App {
    @State private var model = AppModel()

    init() {
        if CommandLine.arguments.contains("--smoke-test") {
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(1))
                let bin = BinaryLocator.standard().find("wing-mixd")?.path(percentEncoded: false) ?? "missing"
                print("WingMix smoke: ok (state dir \(DaemonPaths.standard().stateDir.path(percentEncoded: false)), wing-mixd \(bin))")
                NSApp.terminate(nil)
            }
        }
    }

    var body: some Scene {
        Window("Wing Mix", id: "main") {
            RootView(model: model)
                .frame(minWidth: 980, minHeight: 680)
                .task {
                    guard !CommandLine.arguments.contains("--smoke-test") else { return }
                    let env = ProcessInfo.processInfo.environment
                    if let s = env["WINGMIX_SCREEN"], let sc = Screen.allCases.first(where: { $0.rawValue.lowercased() == s }) {
                        model.screen = sc
                    }
                    await model.start()
                    if let snap = env["WINGMIX_REMIT_SHOT"], !snap.isEmpty, let shots = env["WINGMIX_SHOTS"], !shots.isEmpty {
                        await model.captureRemitBulk(snap: snap, to: shots)
                        return
                    }
                    if let dir = env["WINGMIX_DEMO"], !dir.isEmpty { await model.runDemo(dir: dir) }
                    if let shots = env["WINGMIX_SHOTS"], !shots.isEmpty { await model.captureScreens(to: shots) }
                }
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in
                    model.shutdown()
                }
        }
        .defaultSize(width: 1120, height: 780)
    }
}
