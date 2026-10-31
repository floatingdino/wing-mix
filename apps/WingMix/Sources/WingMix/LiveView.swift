import Charts
import SwiftUI
import WingMixCore

/// Screen 4: target vs current spectrum, meters, problems, plan, move log, confirmations, safety.
struct LiveView: View {
    @Bindable var model: AppModel

    var snap: Snapshot? { model.snapshot }

    var body: some View {
        Scroller {
            VStack(alignment: .leading, spacing: 16) {
                header
                if model.isConnectedToConsole { SlotStripView(model: model) }
                safetyBanner
                HStack(alignment: .top, spacing: 16) {
                    Box("Spectrum: current vs target (dB relative, ERB bands)") {
                        SpectrumChart(points: snap?.spectrum?.points ?? [])
                            .frame(minHeight: 240).padding(6)
                    }
                    Box("Loudness and dynamics") {
                        MetersView(loudness: snap?.loudness, targetLufs: model.targetInfo?["integrated_lufs"]?.double)
                            .padding(6)
                    }
                    .frame(width: 300)
                }
                if !(snap?.pending.isEmpty ?? true) { pendingBox }
                HStack(alignment: .top, spacing: 16) {
                    problemsBox
                    planBox
                }
                HStack(alignment: .top, spacing: 16) {
                    movesBox
                    objectiveBox
                }
                eventsBox
            }
            .padding(20)
        }
    }

    // MARK: header / safety

    var header: some View {
        HStack(spacing: 14) {
            Text("Live").font(.largeTitle.bold())
            Spacer()
            Text(contextLine).font(.caption).foregroundStyle(.secondary)
            if let o = model.status?.session?.objective {
                Text(String(format: "distance to target %.2f", o)).monospacedDigit().foregroundStyle(.secondary)
            }
            if model.sessionRunning {
                Label(model.stopped ? "stopped" : (model.status?.session?.holding.map { "holding: \($0)" } ?? "running"),
                      systemImage: model.stopped ? "pause.circle.fill" : "play.circle.fill")
                    .foregroundStyle(model.stopped ? Color.orange : Color.green)
                Button("End session") { Task { await model.stopSession() } }
            } else {
                Button("Start") { Task { await model.startSession() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(!model.isConnectedToConsole || model.status?.input == nil || model.status?.target == nil)
                    .help(startHelp)
            }
        }
    }

    var contextLine: String {
        let c = model.liveContext.title
        switch snap?.liveContextApplied ?? model.liveContextApplied {
        case true: return "target context: \(c), applied"
        case false: return "target context: \(c), applied once a target is loaded"
        default: return "target context: \(c)"
        }
    }

    var startHelp: String {
        if !model.isConnectedToConsole { return "Connect a console first (Listen)" }
        if model.status?.input == nil { return "Select an input first (Listen)" }
        if model.status?.target == nil { return "Load a target first (Target)" }
        return "Start the \(model.mode.rawValue) session"
    }

    @ViewBuilder var safetyBanner: some View {
        let st = snap?.safety?.status
        let holding = snap?.session?.holding
        let trips = snap?.safety?.trips ?? []
        if st?.feedbackLatched == true || st?.ceilingHold == true || st?.stalled == true || st?.feedbackSuspect == true
            || holding != nil || !trips.isEmpty || model.stopped || snap?.session != nil
        {
            Box {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 8) {
                        Image(systemName: "shield.lefthalf.filled.trianglebadge.exclamationmark").foregroundStyle(.orange)
                        Text("Safety").font(.headline)
                        if model.stopped { badge("STOPPED") }
                        if st?.feedbackLatched == true { badge("feedback latched") }
                        if st?.feedbackSuspect == true { badge("feedback suspected") }
                        if st?.ceilingHold == true { badge("level ceiling hold") }
                        if st?.stalled == true { badge("input stalled") }
                        if let h = holding { badge("holding: \(h)") }
                        Spacer()
                        if model.stopped || st?.feedbackLatched == true {
                            Button("Resume") { Task { await model.resume() } }
                        }
                    }
                    Text(rtaLine).font(.callout).foregroundStyle(.secondary)
                    if let b = snap?.session?.changeBudget {
                        Text(b.display).font(.callout).foregroundStyle(.secondary)
                    }
                    ForEach(Array(trips.enumerated()), id: \.offset) { _, t in
                        Text(String(format: "trip at %.1f s: ", t.atS ?? 0) + model.names.humanize(t.trip?.display ?? "?"))
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }
                .padding(4)
            }
        }
    }

    /// The console RTA feed the feedback guard listens to (`safety.rta_feed`).
    var rtaLine: String {
        let guardOn = snap?.session?.feedbackGuard == true
        if let f = snap?.safety?.rtaFeed {
            let src = f.source.map { model.names.humanize($0.replacingOccurrences(of: "rta:", with: "")) }
            return "Feedback guard on. Console RTA feed: \(f.frames ?? 0) frames" + (src.map { ", listening to \($0)" } ?? "")
        }
        return guardOn ? "Feedback guard on, no console RTA feed (listening to the analysed input only)."
            : "Feedback guard off."
    }

    func badge(_ s: String) -> some View {
        Text(s).font(.caption.bold()).padding(.horizontal, 8).padding(.vertical, 2)
            .background(Capsule().fill(.orange.opacity(0.25)))
    }

    // MARK: lists

    var pendingBox: some View {
        Box("Waiting for your confirmation") {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(snap?.pending ?? []) { p in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(model.names.humanize(p.summary?.label ?? p.key)).font(.body.bold())
                            Text("needs: \(model.names.humanize(p.key))").font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Confirm") { Task { await model.confirm(key: p.key) } }
                    }
                }
            }
            .padding(6)
        }
    }

    var problemsBox: some View {
        Box("Problems (\(snap?.problems.count ?? 0))") {
            VStack(alignment: .leading, spacing: 8) {
                if snap?.problems.isEmpty ?? true { Text("None detected.").foregroundStyle(.secondary) }
                ForEach((snap?.problems ?? []).prefix(8)) { p in
                    VStack(alignment: .leading, spacing: 2) {
                        HStack {
                            Bar(value: (p.severity ?? 0) / 2, tint: .orange).frame(width: 46)
                            Text(model.names.humanize(p.explanation ?? p.id)).font(.callout).lineLimit(2)
                        }
                        if let c = p.contributors, !c.isEmpty {
                            Text("mostly " + c.prefix(3).map { "\(model.names.label($0.strip)) \(Int(($0.share ?? 0) * 100))%" }.joined(separator: ", "))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .padding(6).frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    var planBox: some View {
        Box("Plan (\(snap?.plan.count ?? 0) moves \(model.mode == .autopilot ? "queued" : "suggested"))") {
            VStack(alignment: .leading, spacing: 8) {
                if snap?.plan.isEmpty ?? true { Text("Nothing planned.").foregroundStyle(.secondary) }
                ForEach((snap?.plan ?? []).prefix(8)) { m in
                    VStack(alignment: .leading, spacing: 2) {
                        HStack {
                            Text(model.names.humanize(m.label)).font(.callout.bold())
                            if m.refinement == true {
                                Text("refine").font(.caption2).padding(.horizontal, 4)
                                    .background(Color.blue.opacity(0.15), in: Capsule())
                                    .help("Refinement: nothing is out of tolerance; a small step towards the target")
                            }
                            Spacer()
                            if let g = m.predictedGain { Text(String(format: "-%.3f", g)).monospacedDigit().font(.caption).foregroundStyle(.green) }
                        }
                        if let e = m.explanation { Text(model.names.humanize(e.split(separator: "[").first.map(String.init) ?? e)).font(.caption).foregroundStyle(.secondary).lineLimit(2) }
                        if let n = m.needsConfirmation, !n.isEmpty { Text("needs confirmation: \(model.names.humanize(n.joined(separator: ", ")))").font(.caption).foregroundStyle(.orange) }
                    }
                }
                if let near = snap?.nearMisses, !near.isEmpty {
                    Divider()
                    Text("Held back").font(.caption.bold()).foregroundStyle(.secondary)
                    ForEach(near.prefix(5)) { n in
                        HStack(alignment: .top) {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(model.names.humanize(n.label)).font(.caption)
                                if let r = n.reason { Text(r).font(.caption2).foregroundStyle(.secondary).lineLimit(2) }
                            }
                            Spacer()
                            if let g = n.predictedGain { Text(String(format: "-%.3f", g)).monospacedDigit().font(.caption2).foregroundStyle(.secondary) }
                        }
                    }
                }
            }
            .padding(6).frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    var movesBox: some View {
        Box {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text("Move log").font(.headline)
                    Spacer()
                    Text("\(snap?.touched.count ?? 0) params changed").font(.caption).foregroundStyle(.secondary)
                    Button("Undo last") { Task { await model.undo() } }
                        .disabled(snap?.touched.isEmpty ?? true)
                        .help("Ramp the most recent move back (the daemon undoes the newest move only)")
                }
                if snap?.moves.isEmpty ?? true { Text("No moves made yet.").foregroundStyle(.secondary) }
                ForEach((snap?.moves ?? []).reversed().prefix(10)) { m in
                    HStack(alignment: .top) {
                        Image(systemName: icon(m.outcome)).foregroundStyle(color(m.outcome))
                        VStack(alignment: .leading, spacing: 1) {
                            Text(model.names.humanize(m.summary?.label ?? "batch \(m.seq)")).font(.callout)
                            if let b = m.before, let a = m.after {
                                Text(String(format: "objective %.3f -> %.3f", b, a)).font(.caption).foregroundStyle(.secondary)
                            }
                            if let n = m.note, !n.isEmpty { Text(model.names.humanize(n)).font(.caption).foregroundStyle(.secondary) }
                        }
                        Spacer()
                        Text(m.outcome ?? "").font(.caption).foregroundStyle(.secondary)
                        if m.seq == lastKeptSeq, !(snap?.touched.isEmpty ?? true) {
                            Button("Undo") { Task { await model.undo() } }
                                .controlSize(.small)
                                .help("Ramp this move back. Older moves become undoable once newer ones are undone.")
                        }
                    }
                }
            }
            .padding(6).frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// The newest kept move: the one the daemon's `undo` (a stack) would ramp back.
    var lastKeptSeq: Int? { snap?.moves.last(where: { $0.outcome == "kept" })?.seq }

    func icon(_ o: String?) -> String {
        switch o { case "kept": "checkmark.circle.fill"; case "reverted": "arrow.uturn.backward.circle.fill"
        case "failed": "xmark.circle.fill"; default: "questionmark.circle" }
    }
    func color(_ o: String?) -> Color {
        switch o { case "kept": .green; case "reverted": .orange; case "failed": .red; default: .secondary }
    }

    var objectiveBox: some View {
        Box("Distance to target over time") {
            let pts = snap?.curve ?? []
            if pts.count < 2 {
                Text("Collecting...").foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 100)
            } else {
                Chart(pts) { p in
                    LineMark(x: .value("Audio time (s)", p.atS), y: .value("Objective", p.score?.objective ?? 0))
                    PointMark(x: .value("Audio time (s)", p.atS), y: .value("Objective", p.score?.objective ?? 0))
                        .foregroundStyle(p.kept ?? 0 > 0 ? Color.green : Color.accentColor)
                }
                .chartYScale(domain: .automatic(includesZero: true))
                .frame(minHeight: 120).padding(6)
            }
        }
        .frame(width: 300)
    }

    var eventsBox: some View {
        Box("Actuator events") {
            VStack(alignment: .leading, spacing: 2) {
                if snap?.log.isEmpty ?? true { Text("No actuator activity.").foregroundStyle(.secondary) }
                ForEach(Array((snap?.log ?? []).suffix(12).enumerated()), id: \.offset) { _, e in
                    Text("\(e["type"]?.string ?? "?")  \(e["at_s"]?.double.map { String(format: "%.1f s", $0) } ?? "")")
                        .font(.caption.monospaced()).foregroundStyle(.secondary)
                }
            }
            .padding(6).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Target band (p10-p90), target line and current line on a log frequency axis.
struct SpectrumChart: View {
    var points: [Spectrum.Point]

    var body: some View {
        if points.isEmpty {
            Text("No spectrum yet. Connect a console, pick an input and a target, then start.")
                .foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            Chart {
                ForEach(points) { p in
                    AreaMark(x: .value("Hz", p.hz), yStart: .value("p10", p.lo), yEnd: .value("p90", p.hi),
                             series: .value("Series", "Target range"))
                        .foregroundStyle(.gray.opacity(0.25))
                }
                ForEach(points) { p in
                    LineMark(x: .value("Hz", p.hz), y: .value("dB", p.target), series: .value("Series", "Target"))
                        .foregroundStyle(by: .value("Line", "Target"))
                }
                ForEach(points) { p in
                    LineMark(x: .value("Hz", p.hz), y: .value("dB", p.current), series: .value("Series", "Current"))
                        .foregroundStyle(by: .value("Line", "Current"))
                        .lineStyle(StrokeStyle(lineWidth: 2.5))
                }
            }
            .chartForegroundStyleScale(["Target": Color.gray, "Current": Color.blue])
            .chartXScale(domain: 30...20_000, type: .log)
            .chartXAxis {
                AxisMarks(values: [50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000]) { v in
                    AxisGridLine()
                    AxisValueLabel {
                        if let hz = v.as(Double.self) { Text(hz >= 1000 ? "\(Int(hz / 1000))k" : "\(Int(hz))") }
                    }
                }
            }
            .chartYAxisLabel("dB")
        }
    }
}

/// Horizontal level meters with an optional target tick.
struct MetersView: View {
    var loudness: Loudness?
    var targetLufs: Double?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            meter("Momentary", loudness?.momentaryLufs, unit: "LUFS", range: -40...(-5), target: targetLufs)
            meter("Short-term", loudness?.shortTermLufs, unit: "LUFS", range: -40...(-5), target: targetLufs)
            meter("True peak", loudness?.truePeakDbtp, unit: "dBTP", range: -30...0, target: -1, danger: -1)
            meter("Crest", loudness?.crestDb, unit: "dB", range: 0...25, target: nil)
            meter("Spectral tilt", loudness?.tiltDbPerOct, unit: "dB/oct", range: -8...0, target: nil)
            meter("Width", loudness?.widthDb, unit: "dB", range: -30...0, target: nil)
            meter("Correlation", loudness?.correlation, unit: "", range: -1...1, target: nil)
            if targetLufs != nil { Text("Tick = target loudness").font(.caption2).foregroundStyle(.secondary) }
        }
    }

    func meter(_ name: String, _ v: Double?, unit: String, range: ClosedRange<Double>, target: Double?,
               danger: Double? = nil) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(name).font(.caption)
                Spacer()
                Text(v.map { String(format: "%.1f %@", $0, unit) } ?? "-").font(.caption.monospacedDigit())
            }
            GeometryReader { g in
                let frac = { (x: Double) in CGFloat(min(1, max(0, (x - range.lowerBound) / (range.upperBound - range.lowerBound)))) }
                ZStack(alignment: .leading) {
                    Capsule().fill(.quaternary)
                    if let v {
                        Capsule().fill((danger.map { v > $0 } ?? false) ? Color.red : Color.accentColor)
                            .frame(width: max(4, g.size.width * frac(v)))
                    }
                    if let t = target {
                        Rectangle().fill(.primary.opacity(0.7)).frame(width: 2)
                            .offset(x: g.size.width * frac(t) - 1)
                    }
                }
            }
            .frame(height: 8)
        }
    }
}
