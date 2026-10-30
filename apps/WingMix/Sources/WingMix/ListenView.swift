import AppKit
import SwiftUI
import WingMixCore

/// docs/user/setup-wing-simulator.md: the local checkout when the app knows it, else the repository on GitHub.
func setupGuideURL() -> URL {
    if let root = Bundle.main.object(forInfoDictionaryKey: DaemonPaths.repoRootKey) as? String, !root.isEmpty {
        let f = URL(filePath: root).appending(path: "docs/user/setup-wing-simulator.md")
        if FileManager.default.fileExists(atPath: f.path(percentEncoded: false)) { return f }
    }
    return URL(string: "https://github.com/floatingdino/wing-mix/blob/main/docs/user/setup-wing-simulator.md")!
}

/// Screen 2: which console and which audio input, and which console signal each channel pair carries.
struct ListenView: View {
    @Bindable var model: AppModel

    var body: some View {
        Scroller {
            VStack(alignment: .leading, spacing: 18) {
                Text("Listen").font(.largeTitle.bold())
                consoleBox
                inputBox
            }
            .padding(20)
        }
        .task { if model.consoles.isEmpty { await model.discoverConsoles() } }
    }

    var consoleBox: some View {
        Box("Console") {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    ChoicePicker(title: "Console", selection: $model.selectedConsole,
                                 options: [("Embedded simulator", "sim")]
                                     + model.consoles.filter { $0.kind != "sim" }.map {
                                         ($0.local == true ? $0.title : "\($0.title)  \($0.ip ?? "")", $0.local == true ? "local" : ($0.ip ?? $0.id))
                                     }
                                     + [("WING by IP address...", "ip")])
                    .frame(maxWidth: 420)
                    Button {
                        Task { await model.discoverConsoles() }
                    } label: {
                        Label(model.discovering ? "Searching..." : "Find consoles", systemImage: "dot.radiowaves.left.and.right")
                    }
                    .disabled(model.discovering)
                }
                if model.selectedConsole == "ip" {
                    TextBox(placeholder: "192.168.1.20", text: $model.wingIP).frame(width: 200)
                }
                if model.selectedConsole == "local" {
                    Text("Your running wing-sim, over its native protocol on this Mac (127.0.0.1). Roles and moves use the scene it has loaded right now.")
                        .font(.callout).foregroundStyle(.secondary)
                }
                if model.selectedConsole == "sim" {
                    HStack {
                        ChoicePicker(title: "Scene", selection: $model.simScene,
                                     options: [("Degraded demo (needs mixing)", "degraded"), ("Factory", "factory"),
                                               ("Reference demo", "reference"), ("Scene file (.snap)...", "file")])
                            .frame(width: 280)
                        TextBox(placeholder: "Multitrack WAV (empty = built-in synthetic song)", text: $model.simMultitrack)
                    }
                    if model.simScene == "file" {
                        HStack {
                            TextBox(placeholder: "/path/to/MyShow.snap (e.g. a copy of your wing-sim state.snap)", text: $model.simScenePath)
                            Button("Choose...") { chooseSnap() }
                        }
                    } else {
                        Text("Demo scenes are not your console: to see your own buses, pick your running wing-sim or load your scene file.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                HStack {
                    Button(model.isConnectedToConsole ? "Reconnect" : "Connect") { Task { await model.connectConsole() } }
                        .buttonStyle(.borderedProminent)
                        .disabled((model.selectedConsole == "ip" && model.wingIP.isEmpty)
                            || (model.selectedConsole == "sim" && model.simScene == "file" && model.simScenePath.isEmpty))
                    if model.isConnectedToConsole {
                        Button("Disconnect") { Task { await model.disconnectConsole() } }
                        Label("Connected to \(model.status?.console?.name ?? "console") · scene \(model.status?.console?.scene ?? "?")",
                              systemImage: "checkmark.circle.fill")
                            .foregroundStyle(.green)
                    }
                }
            }
            .padding(6)
        }
    }

    var inputBox: some View {
        Box("Audio input") {
            VStack(alignment: .leading, spacing: 10) {
                ChoicePicker(title: "Source", selection: $model.inputSource,
                             options: InputSource.allCases.map { ($0.rawValue, $0) }, segmented: true)
                .frame(width: 340)
                if model.inputSource == .sim {
                    Text("Analyses the embedded simulator's main bus, solo bus and every input post-fader (fixed taps).")
                        .font(.callout).foregroundStyle(.secondary)
                } else {
                    HStack {
                        ChoicePicker(title: "Device", selection: $model.selectedDevice,
                                     options: model.devices.map { ("\($0.name)  (\($0.maxChannels) ch)", $0.name) })
                        .frame(maxWidth: 480)
                        Button("Refresh") { Task { await model.refreshDevices() } }
                    }
                    if let e = model.deviceError { Text(e).foregroundStyle(.red).font(.caption) }
                    HStack(spacing: 6) {
                        Image(systemName: "info.circle").foregroundStyle(.secondary)
                        Text("The device's input channels are the console's USB outputs. Patch Main 1 L/R and the monitor/solo bus to unused USB outs, then enter those channels here.")
                            .font(.caption).foregroundStyle(.secondary)
                        Link("Setup guide", destination: setupGuideURL()).font(.caption)
                    }
                    tapEditor
                }
                HStack {
                    Button("Use this input") { Task { await model.selectInput() } }
                        .buttonStyle(.borderedProminent)
                        .disabled(!model.isConnectedToConsole)
                    if let i = model.status?.input {
                        Label("\(i.description ?? i.kind)", systemImage: "waveform")
                            .font(.callout).foregroundStyle(.secondary)
                        if let d = i.droppedFrames, d > 0 { Text("\(d) dropped frames").foregroundStyle(.orange).font(.caption) }
                    }
                }
            }
            .padding(6)
        }
    }

    var maxChannels: Int { model.devices.first { $0.name == model.selectedDevice }?.maxChannels ?? 2 }

    var tapEditor: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Which console signal does each channel (pair) carry?").font(.subheadline.bold())
            ForEach($model.taps) { $t in
                HStack {
                    ChoicePicker(title: "Tap", selection: tapKind($t),
                                 options: [("Main LR (compared with the target)", "main"), ("Solo bus", "solo"),
                                           ("Channel strip (post fader)", "input"), ("Monitor feed (feedback guard)", "monitor")])
                        .frame(width: 270)
                    if tapKind($t).wrappedValue == "input" {
                        TextBox(placeholder: "N", text: Binding(get: { String(stripNumber($t).wrappedValue) }, set: { stripNumber($t).wrappedValue = Int($0) ?? 1 })).frame(width: 44)
                    } else if tapKind($t).wrappedValue == "monitor" {
                        TextBox(placeholder: "Bus name", text: monitorName($t)).frame(width: 110)
                    }
                    ChannelStepper(channel: $t.channel, stereo: t.stereo, maxChannel: max(1, maxChannels - (t.stereo ? 1 : 0)))
                        .frame(width: 170)
                    FlagToggle(title: "Stereo pair", isOn: $t.stereo)
                    Button { model.taps.removeAll { $0.id == t.id } } label: { Image(systemName: "minus.circle") }
                        .buttonStyle(.borderless)
                        .disabled(t.tap == "main")
                }
                patchLine(t)
            }
            if model.status?.console?.kind == "wing" {
                Button("Check console USB patch") { Task { await model.refreshUsbPatch() } }.buttonStyle(.link)
            }
            Button { model.taps.append(TapSpec(tap: "solo", channel: nextChannel, stereo: true)) } label: {
                Label("Add tap", systemImage: "plus.circle")
            }
            .buttonStyle(.borderless)
        }
    }

    /// What the console sends to this tap's USB outs, and a warning when it is not what the tap claims.
    @ViewBuilder func patchLine(_ t: TapSpec) -> some View {
        let chans = t.stereo ? [t.channel, t.channel + 1] : [t.channel]
        let got = chans.compactMap { c in model.usbPatch[c].map { "USB \(c) ← \(UsbPatch.describe(grp: $0.grp, input: $0.input))" } }
        if !got.isEmpty {
            Text(got.joined(separator: ",  ")).font(.caption).foregroundStyle(.secondary).padding(.leading, 4)
        }
        if let w = UsbPatch.warning(tap: t.tap, channels: chans, patch: model.usbPatch) {
            Label(w, systemImage: "exclamationmark.triangle.fill").font(.caption).foregroundStyle(.orange)
        }
    }

    func chooseSnap() {
        let p = NSOpenPanel()
        p.allowedContentTypes = []
        p.allowsOtherFileTypes = true
        p.canChooseDirectories = false
        p.message = "Choose a WING scene (.snap)"
        if p.runModal() == .OK, let u = p.url { model.simScenePath = u.path(percentEncoded: false) }
    }

    var nextChannel: Int { min(maxChannels, (model.taps.map { $0.channel + ($0.stereo ? 2 : 1) }.max() ?? 1)) }

    func tapKind(_ t: Binding<TapSpec>) -> Binding<String> {
        Binding {
            let v = t.wrappedValue.tap
            return v.hasPrefix("input") ? "input" : v.hasPrefix("monitor") ? "monitor" : v
        } set: { k in
            t.wrappedValue.tap = k == "input" ? "input.1" : k == "monitor" ? "monitor:IEM 1" : k
        }
    }

    func stripNumber(_ t: Binding<TapSpec>) -> Binding<Int> {
        Binding { Int(t.wrappedValue.tap.dropFirst("input.".count)) ?? 1 } set: { t.wrappedValue.tap = "input.\(max(1, $0))" }
    }

    func monitorName(_ t: Binding<TapSpec>) -> Binding<String> {
        Binding { String(t.wrappedValue.tap.dropFirst("monitor:".count)) } set: { t.wrappedValue.tap = "monitor:\($0)" }
    }
}

struct ChannelStepper: View {
    @Environment(\.shotMode) private var shot
    @Binding var channel: Int
    var stereo: Bool
    var maxChannel: Int

    var body: some View {
        let label = "Channel \(channel)\(stereo ? "-\(channel + 1)" : "")"
        if shot {
            Text(label)
        } else {
            Stepper(label, value: $channel, in: 1...maxChannel)
        }
    }
}
