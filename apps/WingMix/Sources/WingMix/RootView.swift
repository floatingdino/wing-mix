import SwiftUI
import WingMixCore

/// Sidebar of screens + the always-visible control bar (mode, STOP, PANIC).
struct RootView: View {
    @Bindable var model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            ControlBar(model: model)
            Divider()
            if let err = model.lastError {
                HStack {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.yellow)
                    Text(err).lineLimit(2).textSelection(.enabled)
                    Spacer()
                    Button("Dismiss") { model.lastError = nil }.buttonStyle(.link)
                }
                .padding(8)
                .background(.yellow.opacity(0.12))
            }
            NavigationSplitView {
                List(Screen.allCases, selection: $model.screen) { s in
                    Label(s.rawValue, systemImage: s.symbol).tag(s)
                }
                .navigationSplitViewColumnWidth(min: 130, ideal: 150, max: 190)
            } detail: {
                Group {
                    switch model.screen {
                    case .target: TargetView(model: model)
                    case .listen: ListenView(model: model)
                    case .remit: RemitView(model: model)
                    case .live: LiveView(model: model)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
    }
}

/// Mode switch and the big STOP / PANIC buttons; visible on every screen.
struct ControlBar: View {
    @Bindable var model: AppModel

    var body: some View {
        HStack(spacing: 14) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Wing Mix").font(.headline)
                Text(connectionLine).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: 12)
            Picker("Mode", selection: Binding(get: { model.mode }, set: { m in Task { await model.setMode(m) } })) {
                ForEach(MixMode.allCases) { Text($0.rawValue.capitalized).tag($0) }
            }
            .pickerStyle(.segmented)
            .frame(width: 270)
            .disabled(!model.isConnectedToConsole)
            .help("observe: measure only; suggest: plan but never write; autopilot: make the moves")
            if model.stopped {
                Button("Resume") { Task { await model.resume() } }
                    .controlSize(.large)
            }
            Button {
                Task { await model.emergencyStop() }
            } label: {
                Label("STOP", systemImage: "hand.raised.fill").font(.title3.bold()).frame(minWidth: 90)
            }
            .buttonStyle(.borderedProminent).tint(.orange).controlSize(.large)
            .keyboardShortcut(".", modifiers: .command)
            .help("Freeze every move now (Cmd-.)")
            Button {
                Task { await model.panic() }
            } label: {
                Label("PANIC", systemImage: "exclamationmark.octagon.fill").font(.title3.bold()).frame(minWidth: 90)
            }
            .buttonStyle(.borderedProminent).tint(.red).controlSize(.large)
            .help("Ramp every touched parameter back to where it was when the session began")
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
    }

    var connectionLine: String {
        guard model.daemonReady else { return model.daemonMessage }
        let c = model.status?.console.map { "\($0.name ?? $0.kind)" } ?? "no console"
        let t = model.status?.target?.label ?? "no target"
        return "\(c)  ·  \(t)"
    }
}
