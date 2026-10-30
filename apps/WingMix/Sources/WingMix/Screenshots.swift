import AppKit
import SwiftUI

/// `WINGMIX_SHOTS=<dir>` (with `WINGMIX_DEMO`): after the demo session has produced data, render each screen of the
/// main window into `<dir>/<screen>.png` and quit. Used for docs/user/ because `screencapture` needs screen-recording
/// permission that headless runs do not have.
extension AppModel {
    func captureScreens(to dir: String) async {
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try? await Task.sleep(for: .seconds(25))  // let the session measure and plan
        for s in Screen.allCases {
            screen = s
            if s == .target { await refreshStatus() }
            try? await Task.sleep(for: .seconds(2))
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
                try? png.write(to: URL(filePath: dir + "/" + s.rawValue.lowercased() + ".png"))
            }
        }
        NSApp.terminate(nil)
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
