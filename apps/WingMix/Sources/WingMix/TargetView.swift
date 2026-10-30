import SwiftUI
import UniformTypeIdentifiers
import WingMixCore

/// Screen 1: what to aim for: a reference file or a typed description.
struct TargetView: View {
    @Bindable var model: AppModel
    @State private var importing = false
    @State private var dropTargeted = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("Target").font(.largeTitle.bold())
                currentTarget
                GroupBox("Reference mix") {
                    VStack(alignment: .leading, spacing: 12) {
                        dropZone
                        HStack {
                            Toggle("Separate stems (slower, per-instrument targets)", isOn: $model.useStems)
                            Spacer()
                            if model.sectionCount > 1 {
                                Picker("Section", selection: $model.section) {
                                    ForEach(0..<model.sectionCount, id: \.self) { Text("Section \($0 + 1)").tag($0) }
                                }
                                .frame(width: 190)
                            }
                            Button("Load reference") { Task { await model.loadReference() } }
                                .disabled(model.referenceURL == nil || model.loading != nil)
                        }
                        if let l = model.loading {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("\(l.what ?? "Loading"): \(l.stage ?? "")").font(.callout)
                                ProgressView(value: l.progress ?? 0)
                            }
                        }
                    }
                    .padding(6)
                }
                GroupBox("Or describe the mix you want") {
                    VStack(alignment: .leading, spacing: 10) {
                        TextEditor(text: $model.descriptionText)
                            .font(.body).frame(height: 72)
                            .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))
                        HStack {
                            Toggle("Use Claude for unknown phrasing", isOn: $model.useLLM)
                            Spacer()
                            Button("Apply description") { Task { await model.loadDescription() } }
                                .disabled(model.descriptionText.trimmingCharacters(in: .whitespaces).isEmpty
                                    || model.loading != nil)
                        }
                        Text("Example: \"warmer vocal, less harsh cymbals, tighter low end\". Works without Claude; the toggle only adds a fallback for phrases the built-in vocabulary does not know.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    .padding(6)
                }
                GroupBox("Live context") {
                    VStack(alignment: .leading, spacing: 6) {
                        TextField("Room, venue, band, what the audience should hear...", text: $model.liveContext)
                            .disabled(true)
                        Text("Placeholder: live-context targets (M037) are not connected to the daemon yet.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    .padding(6)
                }
            }
            .padding(20)
        }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.audio, .json]) { result in
            if case .success(let url) = result { model.referenceURL = url; model.section = 0 }
        }
    }

    var currentTarget: some View {
        let t = model.status?.target
        return HStack(spacing: 10) {
            Image(systemName: t == nil ? "circle.dashed" : "checkmark.seal.fill")
                .foregroundStyle(t == nil ? Color.secondary : Color.green)
            if let t {
                VStack(alignment: .leading) {
                    Text("Current target: \(t.label ?? t.kind)").font(.headline)
                    Text(targetDetail(t.kind)).font(.caption).foregroundStyle(.secondary)
                }
            } else {
                Text("No target yet. Load a reference or describe the mix.").foregroundStyle(.secondary)
            }
        }
    }

    func targetDetail(_ kind: String) -> String {
        guard let i = model.targetInfo else { return kind }
        if kind == "reference", let l = i["integrated_lufs"]?.double {
            return String(format: "reference  ·  %.1f LUFS integrated  ·  %d section(s)", l, i["sections"]?.int ?? 1)
        }
        if let terms = i["terms"]?.array {
            let names = terms.compactMap { $0["term"]?.string }.joined(separator: ", ")
            let un = i["unrecognised"]?.array?.compactMap(\.string).joined(separator: ", ") ?? ""
            return "description  ·  terms: \(names)" + (un.isEmpty ? "" : "  ·  not understood: \(un)")
        }
        return kind
    }

    var dropZone: some View {
        VStack(spacing: 8) {
            Image(systemName: "arrow.down.doc").font(.largeTitle)
            if let url = model.referenceURL {
                Text(url.lastPathComponent).font(.headline)
                Text(url.deletingLastPathComponent().path(percentEncoded: false)).font(.caption).foregroundStyle(.secondary)
            } else {
                Text("Drop a reference track here").font(.headline)
            }
            Button("Choose file...") { importing = true }
        }
        .frame(maxWidth: .infinity, minHeight: 130)
        .background(RoundedRectangle(cornerRadius: 10)
            .strokeBorder(dropTargeted ? Color.accentColor : Color.secondary.opacity(0.4),
                          style: StrokeStyle(lineWidth: 2, dash: [6])))
        .dropDestination(for: URL.self) { urls, _ in
            guard let u = urls.first else { return false }
            model.referenceURL = u
            model.section = 0
            return true
        } isTargeted: { dropTargeted = $0 }
    }
}
