import SwiftUI
import WingMixCore

/// Screen 3: strips with the role the app detected (monitor, FOH, comms...), confidence and evidence; confirm or
/// override. Confirmed roles are remembered per console and scene.
struct RemitView: View {
    @Bindable var model: AppModel

    var shown: [RoleStrip] {
        guard let r = model.roles else { return [] }
        return r.strips.filter { !model.showOnlyUnconfirmed || ($0.needsConfirmation) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Remit").font(.largeTitle.bold())
                Spacer()
                Button("Refresh") { Task { await model.refreshRoles() } }.disabled(!model.isConnectedToConsole)
            }
            Text("The mix assistant only touches what it is allowed to. Check what each bus and matrix is for; anything it is unsure about stays off limits until you confirm it.")
                .font(.callout).foregroundStyle(.secondary)
            if let r = model.roles {
                HStack {
                    Toggle("Only strips needing confirmation", isOn: $model.showOnlyUnconfirmed)
                    Spacer()
                    Text("\(r.strips.filter(\.needsConfirmation).count) of \(r.strips.count) need confirmation")
                        .font(.callout).foregroundStyle(.secondary)
                    Button("Confirm all shown") { Task { await model.confirmAllShown(shown) } }
                        .buttonStyle(.borderedProminent)
                        .disabled(shown.isEmpty)
                }
                Table(shown) {
                    TableColumn("Strip") { s in
                        VStack(alignment: .leading) {
                            Text(s.name.isEmpty ? s.strip : s.name)
                            if !s.name.isEmpty { Text(s.strip).font(.caption).foregroundStyle(.secondary) }
                        }
                    }
                    .width(min: 90, ideal: 120)
                    TableColumn("Detected") { s in Text(s.detected) }.width(70)
                    TableColumn("Confidence") { s in
                        HStack {
                            ProgressView(value: s.confidence).frame(width: 60)
                            Text(String(format: "%.0f%%", s.confidence * 100)).monospacedDigit().font(.caption)
                        }
                    }
                    .width(110)
                    TableColumn("Evidence") { s in
                        Text(s.evidence.joined(separator: "; ")).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                    }
                    TableColumn("Role") { s in
                        Picker("Role", selection: roleBinding(s)) {
                            ForEach(StripRole.all, id: \.self) { Text($0).tag($0) }
                        }
                        .labelsHidden().frame(width: 120)
                    }
                    .width(130)
                    TableColumn("") { s in
                        if s.needsConfirmation || model.roleOverrides[s.strip] != nil {
                            Button("Confirm") { Task { await model.confirmRoles([s.strip: model.roleOverrides[s.strip] ?? s.proposed]) } }
                        } else {
                            Label("Confirmed", systemImage: "checkmark.circle.fill").foregroundStyle(.green).font(.caption)
                        }
                    }
                    .width(90)
                }
                Text("Settings key: \(r.key). Roles confirmed after a session has started apply from the next session.")
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                ContentUnavailableView("No console connected", systemImage: "checklist",
                                       description: Text("Connect to a console on the Listen screen to see its strips."))
            }
        }
        .padding(20)
        .task { if model.isConnectedToConsole && model.roles == nil { await model.refreshRoles() } }
    }

    func roleBinding(_ s: RoleStrip) -> Binding<String> {
        Binding { model.roleOverrides[s.strip] ?? s.proposed } set: { model.roleOverrides[s.strip] = $0 }
    }
}

extension AppModel {
    func confirmAllShown(_ strips: [RoleStrip]) async {
        var chosen: [String: String] = [:]
        for s in strips where s.needsConfirmation || roleOverrides[s.strip] != nil {
            chosen[s.strip] = roleOverrides[s.strip] ?? s.proposed
        }
        await confirmRoles(chosen)
    }
}
