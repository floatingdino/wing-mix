import SwiftUI
import WingMixCore

/// Mix slots (M064): one button per stored mix (hotkeys 1-9), the reference each was made for and its distance, the
/// active slot highlighted, store / commit / resume tuning, and a diff of two slots (or a slot and the console now).
struct SlotStripView: View {
    @Bindable var model: AppModel
    @Environment(\.shotMode) private var shot
    @State private var renaming: String?
    @State private var newName = ""

    var list: SlotList? { model.slots }
    var comparing: Bool { list?.comparing ?? false }

    var body: some View {
        Box {
            VStack(alignment: .leading, spacing: 8) {
                header
                if list?.slots.isEmpty ?? true {
                    Text("No slots yet. Store the console as it is now; a session that converges is stored automatically, and Original is the console as the session found it.")
                        .font(.callout).foregroundStyle(.secondary)
                } else {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(list?.slots ?? []) { s in slotButton(s) }
                        }
                        .padding(.vertical, 2)
                    }
                }
                footer
                if let d = model.slotDiff { diffView(d) }
            }
            .padding(4)
        }
        .alert("Recall on a live console?", isPresented: Binding(
            get: { model.pendingLiveRecall != nil }, set: { if !$0 { model.pendingLiveRecall = nil } })
        ) {
            Button("Recall \(model.pendingLiveRecall ?? "")", role: .destructive) {
                let n = model.pendingLiveRecall
                model.pendingLiveRecall = nil
                if let n { Task { await model.recallSlot(n, confirmLive: true) } }
            }
            Button("Cancel", role: .cancel) { model.pendingLiveRecall = nil }
        } message: {
            Text("The audience hears the switch at once: it is not ramped like the assistant's moves, and a model change may click. You are asked once per connection.")
        }
        .sheet(isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            VStack(alignment: .leading, spacing: 12) {
                Text("Rename slot \(renaming ?? "")").font(.headline)
                TextField("New name", text: $newName).frame(width: 260)
                HStack {
                    Spacer()
                    Button("Cancel") { renaming = nil }
                    Button("Rename") {
                        let from = renaming ?? ""
                        renaming = nil
                        Task { await model.renameSlot(from, to: newName) }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(newName.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .padding(20)
        }
    }

    var header: some View {
        HStack(spacing: 10) {
            Text("Mix slots").font(.headline)
            if comparing {
                Text("comparing: tuning paused").font(.caption.bold()).padding(.horizontal, 8).padding(.vertical, 2)
                    .background(Capsule().fill(.blue.opacity(0.2)))
            }
            if let t = list?.target?.label {
                Text("active target: \(URL(filePath: t).lastPathComponent)").font(.caption).foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer()
            FlagToggle(title: "Keep my moves", isOn: $model.keepEngineerMoves)
                .help("Params you moved since a slot was stored keep your value when you recall it")
            Button("Store current") { Task { await model.storeSlot() } }
                .help("Store the console as it is now as the next free slot (with the active target)")
            Button("Commit") { Task { await model.commitSlot() } }
                .disabled(!comparing)
                .help("End the comparison on the active slot as one undoable change")
            Button("Resume tuning") { Task { await model.resumeTuning() } }
                .disabled(!comparing && model.mode != .observe)
                .help("Commit and continue the autopilot from the active slot and its target")
        }
    }

    func slotButton(_ s: SlotSummary) -> some View {
        let active = s.active == true || list?.active == s.name
        let ok = s.compatible ?? true
        let key = list?.hotkey(of: s.name)
        return Button {
            Task { await model.recallSlot(s.name) }
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    if let key { Text("\(key)").font(.caption2.monospaced()).foregroundStyle(.secondary) }
                    Text(s.name).font(.title3.bold())
                    if s.auto == true { Image(systemName: "sparkles").font(.caption).foregroundStyle(.secondary) }
                    Spacer(minLength: 0)
                    if !ok { Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange) }
                }
                Text(s.targetLine).font(.caption).lineLimit(1)
                Text(s.detailLine).font(.caption2.monospacedDigit()).foregroundStyle(.secondary).lineLimit(1)
            }
            .frame(width: 150, alignment: .leading)
            .padding(8)
            .background(RoundedRectangle(cornerRadius: 8).fill(active ? Color.accentColor.opacity(0.25) : Color.gray.opacity(0.12)))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(active ? Color.accentColor : Color.clear, lineWidth: 2))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!ok)
        .help(ok ? "Recall \(s.name) (key \(key.map(String.init) ?? "-")): switches at once, not ramped" : (s.refused ?? "incompatible"))
        .modifier(Hotkey(n: key))
        .contextMenu {
            Button("Store current as \(s.name)") { Task { await model.storeSlot(s.name) } }.disabled(s.builtin == true)
            Button("Rename...") {
                newName = s.name
                renaming = s.name
            }.disabled(s.builtin == true)
            Button("Compare with the console") {
                model.diffA = s.name
                model.diffB = "current"
                Task { await model.diffSlots() }
            }
            Divider()
            Button("Delete", role: .destructive) { Task { await model.deleteSlot(s.name) } }.disabled(s.builtin == true)
        }
    }

    var footer: some View {
        HStack(spacing: 10) {
            if let r = model.lastRecall {
                Image(systemName: "bolt.fill").foregroundStyle(.yellow)
                Text(r.summary).font(.caption)
            } else {
                Text("Keys 1-9 switch instantly (not ramped); a model change may click. Tuning pauses while you compare.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            let names = (list?.slots ?? []).map(\.name)
            Text("Differences").font(.caption)
            ChoicePicker(title: "A", selection: $model.diffA, options: names.map { ($0, $0) }).frame(width: 110)
            ChoicePicker(title: "B", selection: $model.diffB, options: [("console now", "current")] + names.map { ($0, $0) })
                .frame(width: 130)
            Button("Show") { Task { await model.diffSlots() } }.disabled(model.diffA.isEmpty)
        }
    }

    func diffView(_ d: SlotDiff) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("\(d.a) vs \(d.b == "current" ? "console now" : d.b): \(d.params.count) params differ").font(.callout.bold())
                Spacer()
                Button("Close") { model.slotDiff = nil }.controlSize(.small)
            }
            if d.params.isEmpty { Text("Identical.").font(.caption).foregroundStyle(.secondary) }
            ForEach(d.byStrip.prefix(shot ? 8 : 40), id: \.strip) { g in
                HStack(alignment: .top, spacing: 8) {
                    Text(model.names.label(g.strip)).font(.caption.bold()).frame(width: 130, alignment: .leading)
                    VStack(alignment: .leading, spacing: 1) {
                        ForEach(g.rows) { r in
                            HStack(spacing: 6) {
                                Text(r.param.split(separator: "/").dropFirst().joined(separator: "/")).font(.caption.monospaced())
                                    .frame(width: 200, alignment: .leading)
                                Text(r.a?.text ?? "-").font(.caption.monospacedDigit()).frame(width: 70, alignment: .trailing)
                                Image(systemName: "arrow.right").font(.caption2).foregroundStyle(.secondary)
                                Text(r.b?.text ?? "-").font(.caption.monospacedDigit()).frame(width: 70, alignment: .leading)
                                if r.engineerMoved == true {
                                    Text("moved by you").font(.caption2).foregroundStyle(.orange)
                                }
                            }
                        }
                    }
                }
            }
            if d.byStrip.count > (shot ? 8 : 40) {
                Text("... and \(d.byStrip.count - (shot ? 8 : 40)) more strips").font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color.gray.opacity(0.08)))
    }
}

/// Bare number keys 1-9 recall slots (the Live screen has no text fields).
private struct Hotkey: ViewModifier {
    var n: Int?
    func body(content: Content) -> some View {
        if let n, let c = "\(n)".first {
            content.keyboardShortcut(KeyEquivalent(c), modifiers: [])
        } else {
            content
        }
    }
}
