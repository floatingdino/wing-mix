import AppKit
import Foundation
import Observation
import WingMixCore

enum Screen: String, CaseIterable, Identifiable {
    case target = "Target", listen = "Listen", remit = "Remit", live = "Live"
    var id: String { rawValue }
    var symbol: String {
        switch self {
        case .target: "scope"
        case .listen: "waveform"
        case .remit: "checklist"
        case .live: "chart.xyaxis.line"
        }
    }
}

enum InputSource: String, CaseIterable, Identifiable {
    case device = "Audio device", sim = "Embedded simulator"
    var id: String { rawValue }
}

/// App state: owns (or attaches to) the daemon, polls it and turns UI actions into control commands.
@Observable
final class AppModel {
    // MARK: daemon
    let paths: DaemonPaths
    let locator: BinaryLocator
    let client: ControlClient
    private var process: Process?
    private var poller: Task<Void, Never>?
    private var streamTask: Task<Void, Never>?
    /// The event stream is connected: refreshes are driven by events and polling is only a fallback.
    var streamConnected = false
    private var refreshing = false
    private var refreshAgain = false
    private var stripsStale = true
    /// Console strip names (`strips`), used everywhere a strip id would be shown.
    var names = StripNames()
    /// Demo/screenshot mode: keep this snapshot instead of polling.
    var frozenSnapshot: Snapshot?
    var daemonReady = false
    var daemonMessage = "Starting daemon..."
    var status: DaemonStatus?
    var snapshot: Snapshot?
    /// Last command error, shown in a banner.
    var lastError: String?

    // MARK: UI state
    var screen: Screen = .target

    // Target
    var referenceURL: URL?
    var useStems = false
    var section = 0
    var targetInfo: JSONValue?
    var sectionCount: Int { max(1, targetInfo?["sections"]?.int ?? 1) }
    var descriptionText = ""
    var useLLM = false
    var liveContext: LiveContext = .live
    /// `applied: false` = the daemon stored it but the target transform is not active yet.
    var liveContextApplied: Bool?
    /// M070 scrutiny (tolerance scale 0.5 strict .. 1.5 forgiving) and refinement mode, as the daemon reports them.
    var scrutiny: Double = 1.0
    var refine = true

    // Listen
    var devices: [AudioDevice] = []
    var deviceError: String?
    var consoles: [ConsoleEntry] = []
    var discovering = false
    var selectedConsole = "sim"
    var wingIP = ""
    var simScene = "degraded"
    /// `simScene == "file"`: a scene file (.snap) loaded into the embedded simulator.
    var simScenePath = ""
    var simMultitrack = ""
    /// Strip names given to the embedded sim (demo/screenshots); empty = the sim's own.
    var simNames: [String] = []
    var inputSource: InputSource = .sim
    var selectedDevice = ""
    var taps: [TapSpec] = [.defaultMain]
    /// What the console patches to the USB outs used by `taps` (device channel = USB out number).
    var usbPatch: [Int: (grp: String, input: Int)] = [:]

    // Remit
    var roles: RoleList?
    var roleOverrides: [String: String] = [:]
    var showOnlyUnconfirmed = false
    /// Remit table filter by strip kind (`all`, `bus`, `input` = inputs + aux, `main` = mains + matrices).
    var remitKindFilter = "all"
    /// Bulk editing (M065): selected strip ids, the shift-click anchor, the change list awaiting confirmation and
    /// the batch that undoes the last applied bulk change.
    var selectedStrips: Set<String> = []
    var selectionAnchor: String?
    var pendingBulk: [RoleBulk.Change] = []
    var pendingBulkTitle = ""
    var lastBulkUndo: JSONValue?
    var lastBulkSummary = ""
    var bulkRole = "monitor"
    var bulkNameMatch = ""
    var bulkRangeKind = "bus"
    var bulkRangeFrom = 8
    var bulkRangeTo = 16
    var acceptThreshold = 0.85

    // Live
    var mode: MixMode = .observe

    // Mix slots (M064)
    var slots: SlotList?
    var lastRecall: SlotRecall?
    /// Params you moved since a slot was stored keep your value on recall (the daemon's default).
    var keepEngineerMoves = true
    var slotDiff: SlotDiff?
    var diffA = ""
    var diffB = "current"
    /// A slot recall waiting for the live-console confirmation (asked once per connection).
    var pendingLiveRecall: String?

    init(paths: DaemonPaths = .standard(), locator: BinaryLocator = .standard()) {
        self.paths = paths
        self.locator = locator
        client = ControlClient(socketURL: paths.socketURL)
    }

    var isConnectedToConsole: Bool { status?.console != nil }
    var sessionRunning: Bool { status?.session?.running ?? false }
    var stopped: Bool { snapshot?.session?.stopped ?? status?.session?.stopped ?? false }
    var loading: DaemonStatus.Loading? { status?.loading }

    // MARK: lifecycle

    /// Attach to a live daemon or start one, then begin polling.
    func start() async {
        if let s = StatusFile.read(paths: paths), s.isLive() {
            daemonMessage = "Attached to wing-mixd \(s.version ?? "") (pid \(s.pid))"
        } else {
            guard let bin = locator.find("wing-mixd") else {
                daemonMessage = "wing-mixd not found (build it with `make build` or set WINGMIX_BIN_DIR)"
                return
            }
            do {
                process = try DaemonProcess.launch(binary: bin, paths: paths)
            } catch {
                daemonMessage = "Could not start wing-mixd: \(error.localizedDescription)"
                return
            }
            let ok = await Self.waitReady(paths)
            guard ok else {
                daemonMessage = "wing-mixd did not start. " + DaemonProcess.logTail(paths: paths, maxLines: 3)
                return
            }
            daemonMessage = "Started wing-mixd (pid \(process?.processIdentifier ?? 0))"
        }
        daemonReady = true
        await refreshStatus()
        await refreshDevices()
        beginStream()
        beginPolling()
        if let s = status {
            if let m = s.mode, let mm = MixMode(rawValue: m) { mode = mm }
            if s.console != nil { await refreshRoles() }
        }
    }

    @concurrent
    private static func waitReady(_ paths: DaemonPaths) async -> Bool {
        DaemonProcess.waitUntilReady(paths: paths)
    }

    /// App is quitting: stop a daemon we started.
    func shutdown() {
        poller?.cancel()
        streamTask?.cancel()
        if process?.isRunning == true {
            _ = try? ControlClient(socketURL: paths.socketURL, timeout: 3).sendLine("quit")
        }
    }

    /// Subscribe to the daemon's events; each one (and each heartbeat) triggers a coalesced refresh.
    private func beginStream() {
        streamTask?.cancel()
        let stream = EventStream(socketURL: paths.socketURL)
        streamTask = Task { [weak self] in
            for await item in stream.items() {
                guard let self else { return }
                switch item {
                case .connected:
                    streamConnected = true
                    stripsStale = true
                    requestRefresh()
                case .event(let e):
                    if e.changesStrips || e.type == "session_started" { stripsStale = true }
                    requestRefresh()
                case .disconnected:
                    streamConnected = false
                }
            }
        }
    }

    /// Refresh now, or once more right after the one in flight (at most ~4 per second).
    func requestRefresh() {
        if refreshing {
            refreshAgain = true
            return
        }
        refreshing = true
        Task {
            repeat {
                refreshAgain = false
                await refreshStatus()
                try? await Task.sleep(for: .milliseconds(250))
            } while refreshAgain
            refreshing = false
        }
    }

    /// Fallback while the stream is down (500 ms); while it is up only the Live screen's meters are polled, since
    /// they are not events (1 s).
    private func beginPolling() {
        poller?.cancel()
        poller = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                if !streamConnected {
                    await refreshStatus()
                    try? await Task.sleep(for: .milliseconds(500))
                } else {
                    if screen == .live, sessionRunning || isConnectedToConsole { await refreshStatus() }
                    try? await Task.sleep(for: .seconds(1))
                }
            }
        }
    }

    func refreshStatus() async {
        do {
            status = try await client.call(DaemonStatus.self, "status")
            daemonReady = true
            if status?.console != nil {
                if frozenSnapshot == nil { snapshot = try? await client.call(Snapshot.self, "snapshot") }
                if stripsStale { await refreshStrips() }
                if let lc = (snapshot?.liveContext ?? status?.liveContext).flatMap(LiveContext.init(rawValue:)), lc != liveContext {
                    liveContext = lc
                }
                if let a = snapshot?.liveContextApplied { liveContextApplied = a }
                if let t = status?.tuning {
                    if let s = t.scrutiny { scrutiny = s }
                    if let r = t.refine { refine = r }
                }
                if let m = snapshot?.mode, let mm = MixMode(rawValue: m), mm != mode { mode = mm }
                if let info = snapshot?.target?.info { targetInfo = info }
                if screen == .live || frozenSnapshot != nil { await refreshSlots() }
            } else {
                snapshot = nil
                names = StripNames()
                stripsStale = true
            }
        } catch {
            daemonReady = false
            daemonMessage = "Daemon not responding: \(error)"
        }
    }

    func refreshStrips() async {
        stripsStale = false
        if let list = try? await client.call(StripList.self, "strips") { names = StripNames(list.strips) }
    }

    // MARK: commands

    /// Run a command, publishing failures to `lastError`.
    @discardableResult
    func send(_ cmd: String, _ args: [String: JSONValue] = [:], timeout: TimeInterval = 30) async -> JSONValue? {
        do {
            let r = try await client.request(cmd, args, timeout: timeout)
            lastError = nil
            return r
        } catch {
            lastError = "\(cmd): \(error)"
            return nil
        }
    }

    /// As `send`, decoding the result.
    func fetch<T: Decodable & Sendable>(_ type: T.Type, _ cmd: String, _ args: [String: JSONValue] = [:]) async -> T? {
        do {
            let r = try await client.call(type, cmd, args)
            lastError = nil
            return r
        } catch {
            lastError = "\(cmd): \(error)"
            return nil
        }
    }

    func setMode(_ m: MixMode) async {
        if await send("set_mode", ["mode": .string(m.rawValue)]) != nil { mode = m }
        await refreshStatus()
    }

    func startSession() async {
        await send("start", ["mode": .string(mode.rawValue)])
        await refreshStatus()
    }

    func stopSession() async {
        await send("stop")
        await refreshStatus()
    }

    func emergencyStop() async {
        await send("STOP", timeout: 5)
        await refreshStatus()
    }

    func panic() async {
        await send("PANIC", timeout: 10)
        mode = .observe
        await refreshStatus()
    }

    func resume() async {
        await send("resume")
        await refreshStatus()
    }

    func undo() async {
        await send("undo")
        await refreshStatus()
    }

    func confirm(key: String) async {
        await send("confirm", ["key": .string(key)])
        await refreshStatus()
    }

    // Mix slots (M064)

    func refreshSlots() async {
        if let l = try? await client.call(SlotList.self, "slot_list") {
            slots = l
            if diffA.isEmpty || !l.slots.contains(where: { $0.name == diffA }) { diffA = l.slots.first?.name ?? "" }
        }
    }

    /// Recall a slot; on a live console the first recall per connection asks first (`pendingLiveRecall`).
    func recallSlot(_ name: String, confirmLive: Bool = false) async {
        if slots?.liveConfirmationNeeded == true, !confirmLive {
            pendingLiveRecall = name
            return
        }
        let args: [String: JSONValue] = ["name": .string(name), "keep_engineer": .bool(keepEngineerMoves),
                                         "confirm_live": .bool(confirmLive)]
        if let r = await fetch(SlotRecall.self, "slot_recall", args) { lastRecall = r }
        await refreshSlots()
        await refreshStatus()
    }

    func storeSlot(_ name: String? = nil) async {
        var args: [String: JSONValue] = [:]
        if let n = name { args["name"] = .string(n) }
        await send("slot_store", args)
        await refreshSlots()
    }

    func commitSlot() async {
        await send("slot_commit")
        await refreshSlots()
        await refreshStatus()
    }

    func resumeTuning() async {
        if let r = await send("slot_resume"), let m = r["mode"]?.string.flatMap(MixMode.init(rawValue:)) { mode = m }
        await refreshSlots()
        await refreshStatus()
    }

    func renameSlot(_ name: String, to: String) async {
        await send("slot_rename", ["name": .string(name), "to": .string(to)])
        await refreshSlots()
    }

    func deleteSlot(_ name: String) async {
        await send("slot_delete", ["name": .string(name)])
        if slotDiff?.a == name || slotDiff?.b == name { slotDiff = nil }
        await refreshSlots()
    }

    func diffSlots() async {
        guard !diffA.isEmpty else { return }
        slotDiff = await fetch(SlotDiff.self, "slot_diff", ["a": .string(diffA), "b": .string(diffB)])
    }

    // Target

    func loadReference() async {
        guard let url = referenceURL else { return }
        var args: [String: JSONValue] = ["path": .string(url.path(percentEncoded: false)), "stems": .bool(useStems)]
        if section > 0 { args["section"] = .number(Double(section)) }
        await send("load_reference", args)
        await refreshStatus()
    }

    /// M070: `set_scrutiny` (scale and/or refinement mode).
    func setScrutiny(scale: Double? = nil, refine r: Bool? = nil) async {
        var args: [String: JSONValue] = [:]
        if let scale { args["scale"] = .number((scale * 20).rounded() / 20) }
        if let r { args["refine"] = .bool(r) }
        if let res = await send("set_scrutiny", args) {
            if let s = res["scrutiny"]?.double { scrutiny = s }
            if let b = res["refine"]?.bool { refine = b }
        }
    }

    func setLiveContext(_ c: LiveContext) async {
        if let r = await send("set_live_context", ["context": .string(c.rawValue)]) {
            liveContext = c
            liveContextApplied = r["applied"]?.bool
        }
    }

    func loadDescription() async {
        let text = descriptionText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        await send("load_description", ["text": .string(text), "llm": .bool(useLLM)])
        await refreshStatus()
    }

    // Listen

    func refreshDevices() async {
        if let list = await fetch(DeviceList.self, "list_devices") {
            devices = list.devices
            deviceError = list.error
            if selectedDevice.isEmpty { selectedDevice = list.devices.first(where: \.default)?.name ?? "" }
        }
    }

    func discoverConsoles() async {
        discovering = true
        defer { discovering = false }
        if let list = await fetch(ConsoleList.self, "list_consoles", ["discover": true, "wait_ms": 1200]) {
            consoles = list.consoles
            if !consoles.contains(where: { $0.id == selectedConsole }) { selectedConsole = consoles.first?.id ?? "sim" }
        }
    }

    func connectConsole() async {
        var args: [String: JSONValue]
        if selectedConsole == "local", let e = consoles.first(where: { $0.local == true }) {
            args = ["console": "wing", "ip": .string(e.ip ?? "127.0.0.1"), "scene": .string(e.scene ?? "running")]
            if let p = e.port { args["port"] = .number(Double(p)) }
        } else if selectedConsole == "sim" {
            let scene = simScene == "file" ? simScenePath : simScene
            var sim: [String: JSONValue] = ["scene": .string(scene), "speed": 1]
            if !simMultitrack.isEmpty { sim["multitrack"] = .string(simMultitrack) }
            if !simNames.isEmpty { sim["names"] = .array(simNames.map { .string($0) }) }
            args = ["console": "sim", "sim": .object(sim)]
        } else {
            args = ["console": .string(selectedConsole == "ip" ? wingIP : selectedConsole)]
        }
        if await send("connect_console", args, timeout: 60) != nil {
            await refreshRoles()
        }
        await refreshStatus()
    }

    func disconnectConsole() async {
        await send("disconnect_console")
        roles = nil
        selectedStrips = []
        lastBulkUndo = nil
        await refreshStatus()
    }

    func selectInput() async {
        var args: [String: JSONValue]
        switch inputSource {
        case .sim:
            args = ["source": "sim"]
        case .device:
            args = ["source": "device", "device": .string(selectedDevice), "taps": .array(taps.map(\.json))]
        }
        await send("select_input", args)
        await refreshStatus()
        await refreshUsbPatch()
    }

    // Remit

    func refreshRoles() async {
        guard let list = await fetch(RoleList.self, "list_roles") else { return }
        if roles?.key != list.key {
            selectedStrips = []
            lastBulkUndo = nil
        }
        roles = list
        roleOverrides = roleOverrides.filter { k, _ in list.strips.contains { $0.strip == k } }
        selectedStrips = selectedStrips.filter { k in list.strips.contains { $0.strip == k } }
    }

    // Bulk role editing

    /// Show `changes` for confirmation (nothing is sent yet).
    func proposeBulk(_ title: String, _ changes: [RoleBulk.Change]) {
        guard !changes.isEmpty else {
            lastError = "\(title): nothing to change for the selection"
            return
        }
        pendingBulkTitle = title
        pendingBulk = changes
    }

    func cancelBulk() {
        pendingBulk = []
    }

    /// Apply the confirmed change list as one `confirm_roles` batch; keep its undo.
    func applyBulk() async {
        let changes = pendingBulk
        pendingBulk = []
        guard !changes.isEmpty else { return }
        let reply = await send("confirm_roles", ["roles": RoleBulk.batch(changes)])
        if reply != nil {
            lastBulkUndo = RoleBulk.undoBatch(fromReply: reply)
            lastBulkSummary = "\(pendingBulkTitle): \(changes.count) strip\(changes.count == 1 ? "" : "s")"
            for c in changes { roleOverrides[c.strip] = nil }
        }
        await refreshRoles()
    }

    /// Undo the last bulk change in one step.
    func undoBulk() async {
        guard let undo = lastBulkUndo else { return }
        if await send("confirm_roles", ["roles": undo]) != nil {
            lastBulkUndo = nil
            lastBulkSummary = ""
        }
        await refreshRoles()
    }

    /// Click on a row: plain = only this one, Cmd = toggle, Shift = range from the anchor (in `order`).
    func clickStrip(_ strip: String, command: Bool, shift: Bool, order: [String]) {
        if shift, let a = selectionAnchor {
            selectedStrips.formUnion(RoleBulk.span(order, from: a, to: strip))
        } else if command {
            if selectedStrips.contains(strip) { selectedStrips.remove(strip) } else { selectedStrips.insert(strip) }
            selectionAnchor = strip
        } else {
            selectedStrips = selectedStrips == [strip] ? [] : [strip]
            selectionAnchor = strip
        }
    }

    /// USB-out patch of the device channels the taps use (WING / wing-sim only; the embedded sim has fixed taps).
    func refreshUsbPatch() async {
        guard status?.console?.kind == "wing" else {
            usbPatch = [:]
            return
        }
        let chans = Array(Set(taps.flatMap { $0.stereo ? [$0.channel, $0.channel + 1] : [$0.channel] })).sorted()
        guard !chans.isEmpty, let r = try? await client.request("params", ["ids": .array(UsbPatch.paramIds(chans).map { .string($0) })]) else {
            usbPatch = [:]
            return
        }
        usbPatch = UsbPatch.parse(r, channels: chans)
    }

    /// Persist `roles` (strip -> role) via `confirm_roles`.
    func confirmRoles(_ chosen: [String: String]) async {
        guard !chosen.isEmpty else { return }
        let obj = chosen.mapValues { JSONValue.string($0) }
        await send("confirm_roles", ["roles": .object(obj)])
        roleOverrides = roleOverrides.filter { chosen[$0.key] == nil }
        await refreshRoles()
    }

    // MARK: demo (screenshots)

    /// `WINGMIX_DEMO=<dir with multitrack.wav + reference.wav>`: connect the embedded sim, load the reference and
    /// start a suggest session, so each screen can be captured against real data.
    func runDemo(dir: String) async {
        simMultitrack = dir + "/multitrack.wav"
        simNames = ["KICK", "SNARE", "HAT", "TOM", "OH L", "OH R", "BASS", "KEYS L", "KEYS R", "GTR", "PAD", "LEAD",
                    "VOX", "BV 1", "BV 2", "SHAKER"]
        selectedConsole = "sim"
        await connectConsole()
        inputSource = .sim
        await selectInput()
        referenceURL = URL(filePath: dir + "/reference.wav")
        await send("load_reference", ["path": .string(dir + "/reference.wav"), "wait": true], timeout: 60)
        await send("set_mode", ["mode": "suggest"])
        mode = .suggest
        await send("start", ["mode": "suggest", "window_s": 8, "time_scale": 0.4])
        await refreshStatus()
    }
}
