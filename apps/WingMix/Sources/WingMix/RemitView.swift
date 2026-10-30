import SwiftUI
import WingMixCore

/// Screen 3: strips with the role the app detected (monitor, FOH, comms...), confidence and evidence; confirm or
/// override. Confirmed roles are remembered per console and scene.
struct RemitView: View {
    @Bindable var model: AppModel
    @Environment(\.shotMode) private var shot

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
                    FlagToggle(title: "Only strips needing confirmation", isOn: $model.showOnlyUnconfirmed)
                    Spacer()
                    Text("\(r.strips.filter(\.needsConfirmation).count) of \(r.strips.count) need confirmation")
                        .font(.callout).foregroundStyle(.secondary)
                    Button("Confirm all shown") { Task { await model.confirmAllShown(shown) } }
                        .buttonStyle(.borderedProminent)
                        .disabled(shown.isEmpty)
                }
                stripList
                Text("Settings key: \(r.key). Confirmed roles apply immediately, also mid-session.")
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                ContentUnavailableView("No console connected", systemImage: "checklist",
                                       description: Text("Connect to a console on the Listen screen to see its strips."))
            }
        }
        .padding(20)
        .task { if model.isConnectedToConsole && model.roles == nil { await model.refreshRoles() } }
    }

    /// Header + one row per strip (the shot-mode cap keeps the screenshot readable).
    @ViewBuilder var stripList: some View {
        let rows = shot ? Array(shown.prefix(14)) : shown
        VStack(spacing: 0) {
            HStack {
                Text("Strip").frame(width: 120, alignment: .leading)
                Text("Detected").frame(width: 80, alignment: .leading)
                Text("Confidence").frame(width: 110, alignment: .leading)
                Text("Evidence").frame(maxWidth: .infinity, alignment: .leading)
                Text("Role").frame(width: 130, alignment: .leading)
                Text("").frame(width: 100)
            }
            .font(.caption.bold()).foregroundStyle(.secondary).padding(.vertical, 6)
            Divider()
            let list = VStack(spacing: 0) {
                ForEach(rows) { s in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(s.name.isEmpty ? s.strip : s.name).lineLimit(1)
                            if !s.name.isEmpty { Text(s.strip).font(.caption).foregroundStyle(.secondary) }
                        }
                        .frame(width: 120, alignment: .leading)
                        Text(s.detected).frame(width: 80, alignment: .leading)
                        HStack {
                            Bar(value: s.confidence).frame(width: 60)
                            Text(String(format: "%.0f%%", s.confidence * 100)).monospacedDigit().font(.caption)
                        }
                        .frame(width: 110, alignment: .leading)
                        Text(s.evidence.joined(separator: "; ")).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        ChoicePicker(title: "Role", selection: roleBinding(s), options: StripRole.all.map { ($0, $0) })
                            .frame(width: 130)
                        Group {
                            if s.needsConfirmation || model.roleOverrides[s.strip] != nil {
                                Button("Confirm") {
                                    Task { await model.confirmRoles([s.strip: model.roleOverrides[s.strip] ?? s.proposed]) }
                                }
                            } else {
                                Label("Confirmed", systemImage: "checkmark.circle.fill").foregroundStyle(.green).font(.caption)
                            }
                        }
                        .frame(width: 100)
                    }
                    .padding(.vertical, 4)
                    Divider()
                }
            }
            if shot { list } else { ScrollView { list } }
        }
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
