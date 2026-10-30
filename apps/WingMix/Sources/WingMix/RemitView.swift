import AppKit
import SwiftUI
import WingMixCore

/// Screen 3: strips with the role the app detected (monitor, FOH, comms...), confidence and evidence; confirm or
/// override, one by one or in bulk (M065: multi-select with Cmd/Shift-click, select by kind/range/name, set role,
/// lock/unlock, accept suggestions above a confidence; every bulk change is listed for confirmation first and can
/// be undone in one step). Confirmed roles are remembered per console and scene.
struct RemitView: View {
    @Bindable var model: AppModel
    @Environment(\.shotMode) private var shot

    var visible: [RoleStrip] {
        guard let r = model.roles else { return [] }
        // routing-only strips (not controllable on this console) are listed only when they are named
        return r.strips.filter { $0.context != true || !$0.name.trimmingCharacters(in: .whitespaces).isEmpty }
    }

    var shown: [RoleStrip] {
        visible.filter { !model.showOnlyUnconfirmed || $0.needsConfirmation }
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
                consoleLine(r)
                if !model.pendingBulk.isEmpty && shot { confirmBox }
                bulkBar
                HStack {
                    FlagToggle(title: "Only strips needing confirmation", isOn: $model.showOnlyUnconfirmed)
                    Spacer()
                    Text("\(visible.filter(\.needsConfirmation).count) of \(visible.count) need confirmation")
                        .font(.callout).foregroundStyle(.secondary)
                    Button("Confirm all shown") { Task { await model.confirmAllShown(shown) } }
                        .buttonStyle(.borderedProminent)
                        .disabled(shown.isEmpty)
                }
                stripList
                Text("Settings key: \(r.key). Confirmed roles apply immediately, also mid-session. Click to select, Cmd-click to add, Shift-click for a range.")
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                ContentUnavailableView("No console connected", systemImage: "checklist",
                                       description: Text("Connect to a console on the Listen screen to see its strips."))
            }
        }
        .padding(20)
        .task { if model.isConnectedToConsole && model.roles == nil { await model.refreshRoles() } }
        .sheet(isPresented: Binding(get: { !shot && !model.pendingBulk.isEmpty },
                                    set: { if !$0 { model.cancelBulk() } })) {
            confirmBox.padding(16).frame(minWidth: 620, minHeight: 300)
        }
    }

    /// Which console and scene the table reflects (and a warning for the embedded sim's demo scenes).
    @ViewBuilder func consoleLine(_ r: RoleList) -> some View {
        let c = r.console
        let kind = c?.kind ?? model.status?.console?.kind ?? "?"
        let name = c?.name ?? model.status?.console?.name ?? "console"
        let scene = c?.scene ?? model.status?.console?.scene ?? "?"
        HStack(spacing: 8) {
            Image(systemName: kind == "sim" ? "cpu" : "slider.vertical.3")
            Text("Showing \(name) · scene \"\(scene)\"").font(.callout.bold())
            if kind == "sim" && ["default", "degraded", "factory", "reference"].contains(scene) {
                Text("This is a built-in demo scene, not your console. Pick your running wing-sim or load your .snap on the Listen screen.")
                    .font(.caption).foregroundStyle(.orange)
            }
        }
    }

    // MARK: bulk

    var strips: [RoleStrip] { visible }
    var sel: Set<String> { model.selectedStrips }

    var bulkBar: some View {
        Box {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Text("Select").font(.subheadline.bold()).frame(width: 60, alignment: .leading)
                    Button("All shown") { model.selectedStrips = Set(shown.map(\.strip)) }
                    Button("Buses") { model.selectedStrips = RoleBulk.ofKind(strips, "bus") }
                    Button("Inputs") { model.selectedStrips = RoleBulk.ofKind(strips, "input").union(RoleBulk.ofKind(strips, "aux")) }
                    Button("Needing confirmation") { model.selectedStrips = RoleBulk.needingConfirmation(strips) }
                    Button("None") { model.selectedStrips = [] }
                    Spacer()
                }
                HStack(spacing: 8) {
                    Text("").frame(width: 60)
                    ChoicePicker(title: "Kind", selection: $model.bulkRangeKind,
                                 options: [("Buses", "bus"), ("Inputs", "input"), ("Aux", "aux"), ("Mains", "main"), ("Matrices", "matrix")])
                        .frame(width: 110)
                    TextBox(placeholder: "8", text: intText($model.bulkRangeFrom)).frame(width: 40)
                    Text("to")
                    TextBox(placeholder: "16", text: intText($model.bulkRangeTo)).frame(width: 40)
                    Button("Select range") {
                        let lo = min(model.bulkRangeFrom, model.bulkRangeTo), hi = max(model.bulkRangeFrom, model.bulkRangeTo)
                        model.selectedStrips = RoleBulk.inRange(strips, kind: model.bulkRangeKind, lo...hi)
                    }
                    Divider().frame(height: 18)
                    TextBox(placeholder: "name contains (e.g. IEM)", text: $model.bulkNameMatch).frame(width: 180)
                    Button("Select matching") { model.selectedStrips = RoleBulk.nameContains(strips, model.bulkNameMatch) }
                        .disabled(model.bulkNameMatch.isEmpty)
                    Spacer()
                }
                HStack(spacing: 8) {
                    Text("\(sel.count) selected").font(.subheadline.bold()).frame(width: 90, alignment: .leading)
                    ChoicePicker(title: "Role", selection: $model.bulkRole, options: StripRole.all.map { ($0, $0) })
                        .frame(width: 120)
                    Button("Set role...") {
                        model.proposeBulk("Set \(sel.count) to \(model.bulkRole)", RoleBulk.setRole(model.bulkRole, strips, sel))
                    }
                    Button("Lock...") { model.proposeBulk("Lock \(sel.count)", RoleBulk.lock(strips, sel)) }
                        .help("Confirm as unknown: the assistant never touches these strips")
                    Button("Unlock...") { model.proposeBulk("Unlock \(sel.count) (accept detected roles)", RoleBulk.unlock(strips, sel)) }
                        .help("Confirm each strip's detected role")
                    Button("Reset to detection...") { model.proposeBulk("Reset \(sel.count) to detection", RoleBulk.reset(strips, sel)) }
                    Spacer()
                }
                .disabled(sel.isEmpty)
                HStack(spacing: 8) {
                    Text("").frame(width: 90)
                    Text("Accept suggestions with confidence ≥")
                    TextBox(placeholder: "85", text: percentText($model.acceptThreshold)).frame(width: 40)
                    Text("%")
                    Button("Accept...") {
                        let within: Set<String>? = sel.isEmpty ? nil : sel
                        model.proposeBulk("Accept suggestions ≥ \(Int(model.acceptThreshold * 100))%\(within == nil ? "" : " in the selection")",
                                          RoleBulk.acceptSuggestions(strips, minConfidence: model.acceptThreshold, within: within))
                    }
                    Spacer()
                    if model.lastBulkUndo != nil {
                        Button {
                            Task { await model.undoBulk() }
                        } label: {
                            Label("Undo \(model.lastBulkSummary)", systemImage: "arrow.uturn.backward")
                        }
                    }
                }
            }
        }
    }

    /// The change list of a pending bulk action: nothing is sent until Apply.
    var confirmBox: some View {
        Box("Confirm: \(model.pendingBulkTitle)") {
            VStack(alignment: .leading, spacing: 6) {
                let unprotect = model.pendingBulk.filter(\.unprotects).count
                Text("\(model.pendingBulk.count) change\(model.pendingBulk.count == 1 ? "" : "s"). Applied together; one Undo reverts them all.")
                    .font(.callout)
                if unprotect > 0 {
                    Label("\(unprotect) strip\(unprotect == 1 ? "" : "s") will no longer be protected: the assistant may change them.",
                          systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange).font(.callout)
                }
                let rows = VStack(alignment: .leading, spacing: 2) {
                    ForEach(model.pendingBulk) { c in
                        HStack {
                            Text(c.name.isEmpty ? c.strip : "\(c.name)").frame(width: 140, alignment: .leading).lineLimit(1)
                            Text(c.strip).font(.caption).foregroundStyle(.secondary).frame(width: 70, alignment: .leading)
                            Text(c.from).frame(width: 90, alignment: .leading)
                            Image(systemName: "arrow.right").font(.caption)
                            Text(c.to ?? "detection (\(c.after))").bold().frame(width: 150, alignment: .leading)
                            if c.unprotects {
                                Text("unprotects").font(.caption).foregroundStyle(.orange)
                            }
                            Spacer()
                        }
                        .font(.callout)
                    }
                }
                if shot { rows } else { ScrollView { rows }.frame(maxHeight: 320) }
                HStack {
                    Spacer()
                    Button("Cancel") { model.cancelBulk() }.keyboardShortcut(.cancelAction)
                    Button("Apply \(model.pendingBulk.count)") { Task { await model.applyBulk() } }
                        .buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction)
                }
            }
        }
    }

    func intText(_ v: Binding<Int>) -> Binding<String> {
        Binding { String(v.wrappedValue) } set: { if let n = Int($0) { v.wrappedValue = n } }
    }

    func percentText(_ v: Binding<Double>) -> Binding<String> {
        Binding { String(Int((v.wrappedValue * 100).rounded())) } set: {
            if let n = Double($0) { v.wrappedValue = min(1, max(0, n / 100)) }
        }
    }

    // MARK: table

    /// Header + one row per strip (the shot-mode cap keeps the screenshot readable).
    @ViewBuilder var stripList: some View {
        let rows = shot ? Array(shown.prefix(18)) : shown
        let order = shown.map(\.strip)
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
                    row(s)
                        .padding(.vertical, 4)
                        .background(sel.contains(s.strip) ? Color.accentColor.opacity(0.16) : Color.clear)
                        .contentShape(Rectangle())
                        .onTapGesture {
                            let f = NSEvent.modifierFlags
                            model.clickStrip(s.strip, command: f.contains(.command), shift: f.contains(.shift), order: order)
                        }
                    Divider()
                }
            }
            if shot { list } else { ScrollView { list } }
        }
    }

    func row(_ s: RoleStrip) -> some View {
        HStack {
            VStack(alignment: .leading) {
                Text(s.name.isEmpty ? s.strip : s.name).lineLimit(1)
                Text(s.strip + (s.context == true ? " · routing only" : "")).font(.caption).foregroundStyle(.secondary)
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
                } else if s.confirmed != nil {
                    Label("Confirmed", systemImage: "checkmark.circle.fill").foregroundStyle(.green).font(.caption)
                } else {
                    Label("Detected", systemImage: "checkmark.circle").foregroundStyle(.secondary).font(.caption)
                }
            }
            .frame(width: 100)
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
