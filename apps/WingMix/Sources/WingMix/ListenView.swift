import SwiftUI
import WingMixCore

/// Screen 2: which console and which audio input, and which console signal each channel pair carries.
struct ListenView: View {
    @Bindable var model: AppModel

    var body: some View {
        ScrollView {
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
        GroupBox("Console") {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Picker("Console", selection: $model.selectedConsole) {
                        Text("Embedded simulator").tag("sim")
                        ForEach(model.consoles.filter { $0.kind != "sim" }) { c in
                            Text("\(c.title)  \(c.ip ?? "")").tag(c.ip ?? c.id)
                        }
                        Text("WING by IP address...").tag("ip")
                    }
                    .frame(maxWidth: 420)
                    Button {
                        Task { await model.discoverConsoles() }
                    } label: {
                        Label(model.discovering ? "Searching..." : "Discover WINGs", systemImage: "dot.radiowaves.left.and.right")
                    }
                    .disabled(model.discovering)
                }
                if model.selectedConsole == "ip" {
                    TextField("192.168.1.20", text: $model.wingIP).frame(width: 200)
                }
                if model.selectedConsole == "sim" {
                    HStack {
                        Picker("Scene", selection: $model.simScene) {
                            Text("Degraded (needs mixing)").tag("degraded")
                            Text("Factory").tag("factory")
                            Text("Reference").tag("reference")
                        }
                        .frame(width: 280)
                        TextField("Multitrack WAV (empty = built-in synthetic song)", text: $model.simMultitrack)
                    }
                }
                HStack {
                    Button(model.isConnectedToConsole ? "Reconnect" : "Connect") { Task { await model.connectConsole() } }
                        .buttonStyle(.borderedProminent)
                        .disabled(model.selectedConsole == "ip" && model.wingIP.isEmpty)
                    if model.isConnectedToConsole {
                        Button("Disconnect") { Task { await model.disconnectConsole() } }
                        Label("Connected to \(model.status?.console?.name ?? "console")", systemImage: "checkmark.circle.fill")
                            .foregroundStyle(.green)
                    }
                }
            }
            .padding(6)
        }
    }

    var inputBox: some View {
        GroupBox("Audio input") {
            VStack(alignment: .leading, spacing: 10) {
                Picker("Source", selection: $model.inputSource) {
                    ForEach(InputSource.allCases) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.segmented).frame(width: 340)
                if model.inputSource == .sim {
                    Text("Analyses the embedded simulator's main bus, solo bus and every input post-fader (fixed taps).")
                        .font(.callout).foregroundStyle(.secondary)
                } else {
                    HStack {
                        Picker("Device", selection: $model.selectedDevice) {
                            ForEach(model.devices) { d in
                                Text("\(d.name)  (\(d.maxChannels) ch)").tag(d.name)
                            }
                        }
                        .frame(maxWidth: 480)
                        Button("Refresh") { Task { await model.refreshDevices() } }
                    }
                    if let e = model.deviceError { Text(e).foregroundStyle(.red).font(.caption) }
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
                    Picker("Tap", selection: tapKind($t)) {
                        Text("Main LR (compared with the target)").tag("main")
                        Text("Solo bus").tag("solo")
                        Text("Channel strip (post fader)").tag("input")
                        Text("Monitor feed (feedback guard)").tag("monitor")
                    }
                    .labelsHidden().frame(width: 270)
                    if tapKind($t).wrappedValue == "input" {
                        TextField("N", value: stripNumber($t), format: .number).frame(width: 44)
                    } else if tapKind($t).wrappedValue == "monitor" {
                        TextField("Bus name", text: monitorName($t)).frame(width: 110)
                    }
                    Stepper("Channel \(t.channel)\(t.stereo ? "-\(t.channel + 1)" : "")",
                            value: $t.channel, in: 1...max(1, maxChannels - (t.stereo ? 1 : 0)))
                        .frame(width: 170)
                    Toggle("Stereo pair", isOn: $t.stereo).toggleStyle(.checkbox)
                    Button { model.taps.removeAll { $0.id == t.id } } label: { Image(systemName: "minus.circle") }
                        .buttonStyle(.borderless)
                        .disabled(t.tap == "main")
                }
            }
            Button { model.taps.append(TapSpec(tap: "solo", channel: nextChannel, stereo: true)) } label: {
                Label("Add tap", systemImage: "plus.circle")
            }
            .buttonStyle(.borderless)
        }
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
