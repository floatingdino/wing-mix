import SwiftUI

/// `ImageRenderer` (used for docs screenshots, see Screenshots.swift) cannot draw AppKit-backed controls, so these
/// wrappers draw a static look-alike when `shotMode` is set and the real control otherwise.
extension EnvironmentValues {
    @Entry var shotMode = false
}

struct ChoicePicker<T: Hashable>: View {
    @Environment(\.shotMode) private var shot
    var title: String
    @Binding var selection: T
    var options: [(String, T)]
    var segmented = false

    var body: some View {
        if shot {
            let sel = options.first { $0.1 == selection }?.0 ?? ""
            if segmented {
                HStack(spacing: 0) {
                    ForEach(options.indices, id: \.self) { i in
                        Text(options[i].0).font(.callout).frame(maxWidth: .infinity).padding(.vertical, 4)
                            .background(options[i].1 == selection ? Color.accentColor.opacity(0.9) : Color.clear)
                            .foregroundStyle(options[i].1 == selection ? Color.white : Color.primary)
                    }
                }
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))
                .clipShape(RoundedRectangle(cornerRadius: 6))
            } else {
                HStack {
                    Text(sel).lineLimit(1)
                    Spacer()
                    Image(systemName: "chevron.up.chevron.down").font(.caption)
                }
                .padding(.horizontal, 8).padding(.vertical, 4)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))
            }
        } else if segmented {
            Picker(title, selection: $selection) {
                ForEach(options.indices, id: \.self) { Text(options[$0].0).tag(options[$0].1) }
            }
            .pickerStyle(.segmented).labelsHidden()
        } else {
            Picker(title, selection: $selection) {
                ForEach(options.indices, id: \.self) { Text(options[$0].0).tag(options[$0].1) }
            }
            .labelsHidden()
        }
    }
}

struct FlagToggle: View {
    @Environment(\.shotMode) private var shot
    var title: String
    @Binding var isOn: Bool

    var body: some View {
        if shot {
            HStack(spacing: 6) {
                Image(systemName: isOn ? "checkmark.square.fill" : "square")
                Text(title)
            }
        } else {
            Toggle(title, isOn: $isOn).toggleStyle(.checkbox)
        }
    }
}

struct TextBox: View {
    @Environment(\.shotMode) private var shot
    var placeholder: String
    @Binding var text: String
    var lines = 1
    var disabled = false

    var body: some View {
        if shot {
            Text(text.isEmpty ? placeholder : text)
                .foregroundStyle(text.isEmpty ? .secondary : .primary)
                .frame(maxWidth: .infinity, minHeight: CGFloat(lines) * 20, alignment: .topLeading)
                .padding(6)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))
        } else if lines > 1 {
            TextEditor(text: $text).font(.body).frame(height: CGFloat(lines) * 24)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))
        } else {
            TextField(placeholder, text: $text).disabled(disabled)
        }
    }
}

/// A vertical `ScrollView` that is just a stack when rendering screenshots.
struct Scroller<Content: View>: View {
    @Environment(\.shotMode) private var shot
    @ViewBuilder var content: Content

    var body: some View {
        if shot { content } else { ScrollView { content } }
    }
}

/// Titled card (stands in for `GroupBox`, which `ImageRenderer` cannot draw).
struct Box<Content: View>: View {
    var title: String?
    @ViewBuilder var content: Content

    init(_ title: String? = nil, @ViewBuilder content: () -> Content) {
        self.title = title
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let title { Text(title).font(.headline) }
            content
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary.opacity(0.35)))
    }
}

/// Thin progress bar (stands in for `ProgressView(value:)`).
struct Bar: View {
    var value: Double
    var tint: Color = .accentColor

    var body: some View {
        GeometryReader { g in
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary)
                Capsule().fill(tint).frame(width: max(3, g.size.width * min(1, max(0, value))))
            }
        }
        .frame(height: 6)
    }
}
