import SwiftUI
import UniformTypeIdentifiers
import WingMixCore

/// Screen 1: what to aim for: a reference file or a typed description.
struct TargetView: View {
    @Bindable var model: AppModel
    @State private var importing = false
    @State private var dropTargeted = false
    @Environment(\.shotMode) private var shot

    var body: some View {
        Scroller {
            VStack(alignment: .leading, spacing: 18) {
                Text("Target").font(.largeTitle.bold())
                currentTarget
                Box("Reference mix") {
                    VStack(alignment: .leading, spacing: 12) {
                        dropZone
                        HStack {
                            FlagToggle(title: "Separate stems (slower, per-instrument targets)", isOn: $model.useStems)
                            Spacer()
                            if model.sectionCount > 1 {
                                ChoicePicker(title: "Section", selection: $model.section, options: (0..<model.sectionCount).map { ("Section \($0 + 1)", $0) })
                                .frame(width: 190)
                            }
                            Button("Load reference") { Task { await model.loadReference() } }
                                .disabled(model.referenceURL == nil || model.loading != nil)
                        }
                        if model.useStems { ToolNotice(model: model, tool: "uv", install: "curl -LsSf https://astral.sh/uv/install.sh | sh") }
                        if let l = model.loading {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("\(l.what ?? "Loading"): \(l.stage ?? "")").font(.callout)
                                Bar(value: l.progress ?? 0)
                            }
                        }
                    }
                    .padding(6)
                }
                Box("Or describe the mix you want") {
                    VStack(alignment: .leading, spacing: 10) {
                        TextBox(placeholder: "Describe the mix you want...", text: $model.descriptionText, lines: 3)
                        HStack {
                            FlagToggle(title: "Use Claude for unknown phrasing", isOn: $model.useLLM)
                            Spacer()
                            Button("Apply description") { Task { await model.loadDescription() } }
                                .disabled(model.descriptionText.trimmingCharacters(in: .whitespaces).isEmpty
                                    || model.loading != nil)
                        }
                        if model.useLLM { ToolNotice(model: model, tool: "claude", install: "npm install -g @anthropic-ai/claude-code") }
                        Text("Example: \"warmer vocal, less harsh cymbals, tighter low end\". Works without Claude; the toggle only adds a fallback for phrases the built-in vocabulary does not know.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    .padding(6)
                }
                Box("Live context") {
                    VStack(alignment: .leading, spacing: 6) {
                        ChoicePicker(title: "Context", selection: Binding<LiveContext>(get: { model.liveContext },
                                                                      set: { c in Task { await model.setLiveContext(c) } }),
                                     options: LiveContext.allCases.map { ($0.title, $0) })
                        .frame(maxWidth: 380)
                        .disabled(!model.daemonReady)
                        Text(model.liveContextApplied == false
                             ? "Stored by the daemon; the target adjustment takes effect once live-context targets (M037/M054) are active."
                             : "Adjusts the target for how the mix will be heard: a room with an audience, a broadcast, or matching a recording.")
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
        .modifier(DropModifier(enabled: !shot, model: model, targeted: $dropTargeted))
    }
}

/// File drop onto the reference zone (not applied while rendering screenshots).
struct DropModifier: ViewModifier {
    var enabled: Bool
    var model: AppModel
    @Binding var targeted: Bool

    func body(content: Content) -> some View {
        if enabled {
            content.dropDestination(for: URL.self) { urls, _ in
                guard let u = urls.first else { return false }
                model.referenceURL = u
                model.section = 0
                return true
            } isTargeted: { targeted = $0 }
        } else {
            content
        }
    }
}

/// "uv not found" style notice: the daemon reports which external tools it can start (`status.tools`).
struct ToolNotice: View {
    var model: AppModel
    var tool: String
    var install: String

    var body: some View {
        if let t = model.status?.missingTool(tool) {
            VStack(alignment: .leading, spacing: 2) {
                Label("`\(tool)` not found by the daemon: \(t.for ?? "this feature") is unavailable.", systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange).font(.callout)
                Text("Install it (\(install)), or put it in ~/.local/bin or /opt/homebrew/bin, then restart Wing Mix.")
                    .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
        }
    }
}
