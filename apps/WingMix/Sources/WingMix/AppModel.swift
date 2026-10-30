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
    var liveContext = ""

    // Listen
    var devices: [AudioDevice] = []
    var deviceError: String?
    var consoles: [ConsoleEntry] = []
    var discovering = false
    var selectedConsole = "sim"
    var wingIP = ""
    var simScene = "degraded"
    var simMultitrack = ""
    var inputSource: InputSource = .sim
    var selectedDevice = ""
    var taps: [TapSpec] = [.defaultMain]

    // Remit
    var roles: RoleList?
    var roleOverrides: [String: String] = [:]
    var showOnlyUnconfirmed = false

    // Live
    var mode: MixMode = .observe

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
        if process?.isRunning == true {
            _ = try? ControlClient(socketURL: paths.socketURL, timeout: 3).sendLine("quit")
        }
    }

    private func beginPolling() {
        poller?.cancel()
        poller = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refreshStatus()
                try? await Task.sleep(for: .milliseconds(500))
            }
        }
    }

    func refreshStatus() async {
        do {
            status = try await client.call(DaemonStatus.self, "status")
            daemonReady = true
            if status?.console != nil {
                snapshot = try? await client.call(Snapshot.self, "snapshot")
                if let m = snapshot?.mode, let mm = MixMode(rawValue: m), mm != mode { mode = mm }
                if let info = snapshot?.target?.info { targetInfo = info }
            } else {
                snapshot = nil
            }
        } catch {
            daemonReady = false
            daemonMessage = "Daemon not responding: \(error)"
        }
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

    // Target

    func loadReference() async {
        guard let url = referenceURL else { return }
        var args: [String: JSONValue] = ["path": .string(url.path(percentEncoded: false)), "stems": .bool(useStems)]
        if section > 0 { args["section"] = .number(Double(section)) }
        await send("load_reference", args)
        await refreshStatus()
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
        if selectedConsole == "sim" {
            var sim: [String: JSONValue] = ["scene": .string(simScene), "speed": 1]
            if !simMultitrack.isEmpty { sim["multitrack"] = .string(simMultitrack) }
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
    }

    // Remit

    func refreshRoles() async {
        guard let list = await fetch(RoleList.self, "list_roles") else { return }
        roles = list
        roleOverrides = roleOverrides.filter { k, _ in list.strips.contains { $0.strip == k } }
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
