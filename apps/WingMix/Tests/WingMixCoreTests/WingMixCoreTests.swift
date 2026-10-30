import Foundation
import Testing
@testable import WingMixCore

/// Repo root (this file is apps/WingMix/Tests/WingMixCoreTests/...).
let repoRoot = URL(filePath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent()
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()

func fixture(_ name: String) throws -> Data {
    try Data(contentsOf: URL(filePath: #filePath).deletingLastPathComponent().appending(path: "Fixtures/\(name)"))
}

/// Replies are one line each.
func oneLine(_ s: String) -> String { s.replacingOccurrences(of: "\n", with: "") }

func tempDir(_ tag: String) throws -> URL {
    // short path: sockaddr_un holds 104 bytes
    let d = URL(filePath: "/tmp/wmx-\(getpid())-\(tag)", directoryHint: .isDirectory)
    try? FileManager.default.removeItem(at: d)
    try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
    return d
}

@Suite struct ProtocolTests {
    @Test func `request lines carry the command and arguments`() throws {
        let line = ControlClient.requestLine("set_mode", ["mode": "suggest"])
        #expect(line == #"{"cmd":"set_mode","mode":"suggest"}"#)
        let sim = ControlClient.requestLine("connect_console", ["console": "sim", "sim": ["speed": 1, "scene": "degraded"]])
        #expect(sim == #"{"cmd":"connect_console","console":"sim","sim":{"scene":"degraded","speed":1}}"#)
    }

    @Test func `ok replies yield the result and errors throw`() throws {
        let r = try ControlClient.parseReply(#"{"ok":true,"id":1,"result":{"mode":"suggest"}}"# + "\n")
        #expect(r["mode"]?.string == "suggest")
        #expect(try ControlClient.parseReply(#"{"ok":true}"#) == .null)
        #expect(throws: ControlError.daemon("no console")) {
            try ControlClient.parseReply(#"{"ok":false,"error":"no console"}"#)
        }
        #expect(throws: ControlError.badReply("(no reply)")) { try ControlClient.parseReply("") }
        #expect(throws: ControlError.self) { try ControlClient.parseReply("ok fine") }
    }

    @Test func `socket paths that do not fit sockaddr_un are refused`() {
        let c = ControlClient(socketURL: URL(filePath: "/tmp/" + String(repeating: "a", count: 200) + "/control.sock"))
        #expect(throws: ControlError.self) { try c.sendLine("ping") }
    }

    @Test func `a missing socket is an io error`() throws {
        let d = try tempDir("nosock")
        let c = ControlClient(socketURL: d.appending(path: "control.sock"), timeout: 1)
        #expect(throws: ControlError.self) { try c.sendLine("ping") }
    }
}

@Suite struct ModelTests {
    @Test func `decodes the daemon's snapshot`() throws {
        let raw = String(decoding: try fixture("snapshot-reply.json"), as: UTF8.self)
        let s = try ControlClient.parseReply(raw, as: Snapshot.self)
        #expect(s.mode == "suggest")
        #expect(s.console?.kind == "sim")
        #expect(s.target?.label == "reference.wav")
        #expect(s.loudness?.shortTermLufs != nil)
        let spec = try #require(s.spectrum)
        #expect(spec.erbHz.count == 40 && spec.points.count == 40)
        #expect(spec.points.allSatisfy { $0.lo <= $0.hi + 0.001 })
        #expect(!s.problems.isEmpty && s.problems[0].explanation != nil)
        #expect(!s.plan.isEmpty)
        #expect(s.plan[0].changes?.first?.param == "input.1/fader")
        #expect(s.plan[0].changes?.first?.text == "input.1/fader: -4.0 -> -7.0")
        #expect(s.curve.count >= 2)
        #expect(s.safety?.status?.feedbackLatched == false)
        #expect(s.touched.isEmpty && s.moves.isEmpty && s.pending.isEmpty)
    }

    @Test func `decodes the change budget`() throws {
        let raw = """
        {"ok":true,"result":{"mode":"autopilot","curve":[],"session":{"running":true,"budget":[7,12.5],
         "change_budget":{"batches":7,"max_batches":100,"net_db":12.5,"max_net_db":60.0,"rate_db":6.0,
         "max_db_per_min":24.0,"travel_db":20.0,"paced_until_s":80.0}}}}
        """
        let s = try ControlClient.parseReply(oneLine(raw), as: Snapshot.self)
        let b = try #require(s.session?.changeBudget)
        #expect(b.netDb == 12.5 && b.maxDbPerMin == 24 && b.batches == 7)
        #expect(b.display == "change budget: net 12.5 of 60 dB, 6.0 of 24 dB in the last minute (pacing)")
    }

    @Test func `decodes move records, pending confirmations and trips`() throws {
        let raw = """
        {"ok":true,"result":{"mode":"autopilot","curve":[],
         "moves":[{"seq":3,"at_s":40.5,"summary":{"label":"vox fader +1 dB","strip":"input.13"},"outcome":"kept","before":1.6,"after":1.5,"note":""}],
         "pending":[{"key":"bus.5","summary":{"label":"IEM trim","strip":"bus.5","needs_confirmation":["bus.5"]}}],
         "safety":{"status":{"feedback_latched":true,"feedback_suspect":false,"ceiling_hold":false,"stalled":false},
                   "trips":[{"at_s":12.0,"trip":"feedback","reverted":[]}]},
         "touched":[{"param":"input.13/fader","baseline":{"type":"float","value":-10.0},"current":{"type":"float","value":-9.0}}]}}
        """
        let s = try ControlClient.parseReply(oneLine(raw), as: Snapshot.self)
        #expect(s.moves.first?.outcome == "kept" && s.moves.first?.summary?.label == "vox fader +1 dB")
        #expect(s.pending.first?.key == "bus.5")
        #expect(s.safety?.status?.feedbackLatched == true)
        #expect(s.safety?.trips.first?.trip?.string == "feedback")
        #expect(s.touched.first?.current?.text == "-9.0")
        #expect(s.problems.isEmpty && s.spectrum == nil)
    }

    @Test func `decodes role lists and picks what to offer`() throws {
        let raw = """
        {"ok":true,"result":{"key":"sim|default","strips":[
         {"strip":"bus.10","name":"IEM 1","detected":"monitor","effective":"unknown","confidence":0.38,"evidence":["feeds a main directly"],"confirmed":null,"needs_confirmation":true},
         {"strip":"bus.2","name":"","detected":"fx","effective":"fx","confidence":0.9,"evidence":[],"confirmed":"fx","needs_confirmation":false}]}}
        """
        let r = try ControlClient.parseReply(oneLine(raw), as: RoleList.self)
        #expect(r.strips[0].proposed == "monitor" && r.strips[0].needsConfirmation)
        #expect(r.strips[1].proposed == "fx" && !r.strips[1].needsConfirmation)
        #expect(StripRole.all.contains("monitor"))
    }

    @Test func `live context values match the protocol`() throws {
        #expect(LiveContext.allCases.map(\.rawValue) == ["live", "broadcast_video", "match_record"])
        let s = try ControlClient.parseReply(#"{"ok":true,"result":{"mode":"observe","live_context":"match_record"}}"#,
                                             as: Snapshot.self)
        #expect(s.liveContext == "match_record")
    }

    @Test func `tap specs serialise to the select_input shape`() {
        let t = TapSpec(tap: "input.5", channel: 7, stereo: false).json
        #expect(t["tap"]?.string == "input.5" && t["channels"]?.array == [.number(7)])
        #expect(TapSpec.defaultMain.json["channels"]?.array == [.number(1), .number(2)])
    }

    @Test func `decodes devices and consoles`() throws {
        let d = try ControlClient.parseReply(
            #"{"ok":true,"result":{"devices":[{"name":"Scarlett","default":true,"max_channels":2,"default_sample_rate":48000}],"sim":{"taps":["main"]}}}"#,
            as: DeviceList.self)
        #expect(d.devices[0].name == "Scarlett" && d.devices[0].default && d.sim?.taps == ["main"])
        let c = try ControlClient.parseReply(
            #"{"ok":true,"result":{"consoles":[{"kind":"sim"},{"kind":"wing","ip":"10.0.0.5","name":"WING-1","model":"WING"}]}}"#,
            as: ConsoleList.self)
        #expect(c.consoles.map(\.id) == ["sim", "10.0.0.5"])
    }
}

@Suite struct StripNameTests {
    let names = StripNames([
        StripEntry(id: "input.13", kind: "input", name: "VOX 1"),
        StripEntry(id: "bus.5", kind: "bus", name: " IEM 1 "),
        StripEntry(id: "input.2", kind: "input", name: "  "),
    ])

    @Test func `labels carry the console name and the short id`() {
        #expect(names.label("input.13") == "VOX 1 (ch 13)")
        #expect(names.label("bus.5") == "IEM 1 (bus 5)")
        #expect(names.label("input.2") == "ch 2")  // blank name = unnamed
        #expect(names.label("matrix.3") == "mtx 3" && names.label("dca.1") == "DCA 1")
        #expect(names.label("weird") == "weird")
    }

    @Test func `ids inside free text and param paths are rewritten`() {
        #expect(names.humanize("input.13 fader +1.6 dB") == "VOX 1 (ch 13) fader +1.6 dB")
        #expect(names.humanize("input.13/fader: -10.0 -> -7.0") == "VOX 1 (ch 13) fader: -10.0 -> -7.0")
        #expect(names.humanize("bus.5/eq/1/gain") == "IEM 1 (bus 5) eq/1/gain")
        #expect(names.humanize("mostly input.13 and input.7, not mix_band") == "mostly VOX 1 (ch 13) and ch 7, not mix_band")
        #expect(names.humanize("no ids here") == "no ids here")
        #expect(StripNames().humanize("input.13") == "ch 13")
    }

    @Test func `decodes the strips reply`() throws {
        let raw = #"{"ok":true,"result":{"strips":[{"id":"input.1","kind":"input","name":"KICK"}]}}"#
        let l = try ControlClient.parseReply(raw, as: StripList.self)
        #expect(StripNames(l.strips).label("input.1") == "KICK (ch 1)")
    }
}

@Suite struct EventStreamTests {
    @Test func `line buffer splits chunks into lines`() {
        var b = LineBuffer()
        #expect(b.append(Array("{\"a\":1}\n{\"b".utf8)) == ["{\"a\":1}"])
        #expect(b.append(Array("\":2}\n\n".utf8)) == ["{\"b\":2}", ""])
    }

    @Test func `events parse and skip junk`() {
        let e = DaemonEvent.parse(#"{"source":"daemon","type":"console_connected","at_s":1.5,"unix":1}"#)
        #expect(e?.type == "console_connected" && e?.atS == 1.5 && e?.changesStrips == true)
        #expect(DaemonEvent.parse(#"{"source":"control","type":"measured"}"#)?.changesStrips == false)
        #expect(DaemonEvent.parse("nonsense") == nil)
    }

    @Test func `stream reconnects when the daemon is not there yet`() async throws {
        let dir = try tempDir("evs")
        defer { try? FileManager.default.removeItem(at: dir) }
        let stream = EventStream(socketURL: DaemonPaths(stateDir: dir).socketURL, maxBackoff: 0.2)
        var seen: [StreamItem] = []
        for await item in stream.items() {
            seen.append(item)
            if seen.count == 2 { break }
        }
        #expect(seen.count == 2)
        guard case .disconnected = seen[0], case .disconnected = seen[1] else {
            Issue.record("expected repeated disconnects, got \(seen)")
            return
        }
    }
}

@Suite struct StatusTests {
    let json = """
    {"running":true,"pid":4242,"version":"0.1.0","updated_unix":1000.0,"uptime_s":3,"state_dir":"/x",
     "console":{"kind":"sim","name":"wing-sim","key":"sim","scene":"default"},
     "input":{"kind":"sim","description":"sim taps","sample_rate":48000,"taps":["main"],"dropped_frames":0,"clock_s":1.5,"stream_errors":0},
     "target":{"kind":"reference","label":"ref.wav"},"loading":{"what":"reference","stage":"analysing","progress":0.4},
     "mode":"suggest","session":{"running":true,"stopped":false,"holding":null,"objective":1.2,"batches":0,"pending":0,"touched":0},
     "safety":{"status":{"feedback_latched":false,"feedback_suspect":false,"ceiling_hold":false,"stalled":false},"trips":0},
     "last_error":null}
    """

    @Test func `decodes status.json`() throws {
        let s = try DaemonStatus.decode(Data(json.utf8))
        #expect(s.pid == 4242 && s.console?.kind == "sim" && s.target?.label == "ref.wav")
        #expect(s.loading?.progress == 0.4 && s.session?.objective == 1.2 && s.input?.sampleRate == 48000)
    }

    @Test func `liveness needs running, a fresh document and the process`() throws {
        var s = try DaemonStatus.decode(Data(json.utf8))
        let now = Date(timeIntervalSince1970: 1002)
        #expect(s.isLive(now: now, processExists: { _ in true }))
        #expect(!s.isLive(now: now, processExists: { _ in false }))
        #expect(!s.isLive(now: Date(timeIntervalSince1970: 1100), processExists: { _ in true }))
        s.running = false
        #expect(!s.isLive(now: now, processExists: { _ in true }))
    }

    @Test func `state dir comes from the environment and never defaults to WingSimulator`() {
        #expect(DaemonPaths.standard(environment: ["WINGMIX_STATE_DIR": "/tmp/x"]).stateDir.path == "/tmp/x")
        let std = DaemonPaths.standard(environment: [:]).stateDir.path
        #expect(std.hasSuffix("Library/Application Support/WingMix"))
        #expect(!std.contains("WingSimulator"))
    }

    @Test func `daemon command line passes the state dir`() {
        let p = DaemonPaths(stateDir: URL(filePath: "/tmp/s"))
        #expect(DaemonProcess.arguments(paths: p) == ["--state-dir", "/tmp/s"])
        #expect(p.socketURL.lastPathComponent == "control.sock")
    }

    @Test func `binary locator finds the first executable`() throws {
        let d = try tempDir("bin")
        let exe = d.appending(path: "wing-mixd")
        try "#!/bin/sh\n".write(to: exe, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: exe.path)
        let loc = BinaryLocator(searchDirs: [d.appending(path: "none"), d])
        #expect(loc.find("wing-mixd") == exe)
        #expect(loc.find("nope") == nil)
    }
}

/// The real daemon over the real socket with the embedded sim (needs `cargo build --release -p wing-mixd`;
/// `make app` builds it first). Skipped when the binary is absent.
let daemonBinary = repoRoot.appending(path: "target/release/wing-mixd")
let daemonExists = FileManager.default.isExecutableFile(atPath: daemonBinary.path(percentEncoded: false))

@Suite(.serialized) struct DaemonIntegrationTests {
    @Test(.enabled(if: daemonExists))
    func `client drives wing-mixd against the embedded sim`() async throws {
        let dir = try tempDir("int")
        defer { try? FileManager.default.removeItem(at: dir) }
        let paths = DaemonPaths(stateDir: dir)
        let proc = try DaemonProcess.launch(binary: daemonBinary, paths: paths)
        defer { if proc.isRunning { proc.terminate() } }
        #expect(DaemonProcess.waitUntilReady(paths: paths))
        let client = ControlClient(socketURL: paths.socketURL)

        let ping = try await client.request("ping")
        #expect(ping["name"]?.string == "wing-mixd")
        let consoles = try await client.call(ConsoleList.self, "list_consoles", ["discover": false, "local": false])
        #expect(consoles.consoles.first?.kind == "sim")
        await #expect(throws: ControlError.self) { try await client.request("start") }

        _ = try await client.request("connect_console", ["console": "sim", "sim": ["scene": "degraded", "speed": 0]])
        _ = try await client.request("select_input", ["source": "sim"])
        let roles = try await client.call(RoleList.self, "list_roles")
        #expect(roles.strips.count >= 16)
        let desc = try await client.request("load_description", ["text": "warmer vocal", "wait": true])
        #expect(desc["kind"]?.string == "description")
        _ = try await client.request("set_mode", ["mode": "suggest"])
        let lc = try await client.request("set_live_context", ["context": "broadcast_video"])
        #expect(lc["live_context"]?.string == "broadcast_video")

        let status = try await client.call(DaemonStatus.self, "status")
        #expect(status.liveContext == "broadcast_video")
        #expect(status.console?.kind == "sim" && status.mode == "suggest" && status.target?.kind == "description")
        let fileStatus = try #require(StatusFile.read(paths: paths))
        #expect(fileStatus.running && fileStatus.pid == Int(proc.processIdentifier))
        let snap = try await client.call(Snapshot.self, "snapshot")
        #expect(snap.console?.kind == "sim")
        #expect(snap.liveContextApplied == true && snap.liveContext == "broadcast_video")
        let strips = try await client.call(StripList.self, "strips")
        #expect(StripNames(strips.strips).names.count >= 16)

        // event stream: a console change arrives as an event
        let stream = EventStream(socketURL: paths.socketURL)
        let gotEvent = Task { () -> Bool in
            for await item in stream.items() {
                if case .event(let e) = item, e.type == "live_context_changed", e.raw["live_context"]?.string == "live" { return true }
            }
            return false
        }
        try await Task.sleep(for: .milliseconds(500))
        _ = try await client.request("set_live_context", ["context": "live"])
        let timeout = Task { try await Task.sleep(for: .seconds(10)); gotEvent.cancel() }
        #expect(await gotEvent.value)
        timeout.cancel()

        // bulk role editing (M065): one batch, one undo
        let before = try await client.call(RoleList.self, "list_roles")
        let buses = RoleBulk.inRange(before.strips, kind: "bus", 8...16)
        let lock = RoleBulk.lock(before.strips, buses)
        #expect(lock.count == 9)
        let reply = try await client.request("confirm_roles", ["roles": RoleBulk.batch(lock)])
        let locked = try await client.call(RoleList.self, "list_roles")
        #expect(locked.strips.filter { buses.contains($0.strip) }.allSatisfy { $0.confirmed == "unknown" })
        let undo = try #require(RoleBulk.undoBatch(fromReply: reply))
        _ = try await client.request("confirm_roles", ["roles": undo])
        let restored = try await client.call(RoleList.self, "list_roles")
        #expect(restored == before)
        #expect(restored.console?.kind == "sim")

        // no session yet: STOP and PANIC are refused with a message, not a hang
        await #expect(throws: ControlError.daemon("no session: nothing to stop")) { try await client.request("STOP") }
        _ = try await client.request("quit")
        proc.waitUntilExit()
        #expect(!FileManager.default.fileExists(atPath: paths.socketURL.path(percentEncoded: false)))
    }
}

@Suite struct BulkRoleTests {
    func strip(_ id: String, _ name: String, detected: String, effective: String, conf: Double,
               confirmed: String? = nil, needs: Bool = false) -> RoleStrip {
        RoleStrip(strip: id, name: name, detected: detected, effective: effective, confidence: conf, evidence: [],
                  confirmed: confirmed, needsConfirmation: needs, context: nil)
    }

    var strips: [RoleStrip] {
        [strip("bus.1", "VOX VERB", detected: "fx", effective: "unknown", conf: 0.71, needs: true),
         strip("bus.6", "DRUMS FOH", detected: "foh_group", effective: "unknown", conf: 0.92, needs: true),
         strip("bus.8", "VOX 1 IEM", detected: "monitor", effective: "monitor", conf: 1.0),
         strip("bus.9", "Keys iem", detected: "monitor", effective: "monitor", conf: 1.0, confirmed: "monitor"),
         strip("bus.16", "COMMS", detected: "comms", effective: "comms", conf: 1.0),
         strip("input.1", "KICK", detected: "foh", effective: "foh", conf: 1.0),
         strip("bus.12", "", detected: "unknown", effective: "unknown", conf: 0.5, needs: true)]
    }

    @Test func `selections by kind, range, name, span`() {
        #expect(RoleBulk.ofKind(strips, "bus").count == 6)
        #expect(RoleBulk.inRange(strips, kind: "bus", 8...16) == ["bus.8", "bus.9", "bus.16", "bus.12"])
        #expect(RoleBulk.nameContains(strips, "iem") == ["bus.8", "bus.9"])
        #expect(RoleBulk.nameContains(strips, "  ").isEmpty)
        #expect(RoleBulk.needingConfirmation(strips) == ["bus.1", "bus.6", "bus.12"])
        let order = strips.map(\.strip)
        #expect(RoleBulk.span(order, from: "bus.16", to: "bus.6") == ["bus.6", "bus.8", "bus.9", "bus.16"])
        #expect(["bus.10", "main.1", "bus.2", "input.3", "aux.1"].sorted(by: RoleBulk.consoleOrder)
            == ["input.3", "aux.1", "bus.2", "bus.10", "main.1"])
    }

    @Test func `bulk actions list only real changes and flag unprotection`() {
        let all = Set(strips.map(\.strip))
        let set = RoleBulk.setRole("monitor", strips, RoleBulk.inRange(strips, kind: "bus", 8...16))
        // bus.9 already confirmed monitor: no-op
        #expect(Set(set.map(\.strip)) == ["bus.8", "bus.16", "bus.12"])
        #expect(set.allSatisfy { !$0.unprotects })
        let lock = RoleBulk.lock(strips, ["bus.1", "bus.8"])
        #expect(lock.map(\.to) == [Optional("unknown"), Optional("unknown")])
        let unlock = RoleBulk.unlock(strips, all)
        #expect(!unlock.contains { $0.strip == "bus.12" }, "unknown detection: nothing to unlock to")
        #expect(unlock.first { $0.strip == "bus.1" }?.unprotects == true)
        let reset = RoleBulk.reset(strips, all)
        #expect(reset.map(\.strip) == ["bus.9"] && reset[0].to == nil && reset[0].after == "monitor")
        let acc = RoleBulk.acceptSuggestions(strips, minConfidence: 0.85)
        #expect(acc.map(\.strip) == ["bus.6"] && acc[0].to == "foh_group")
        #expect(RoleBulk.acceptSuggestions(strips, minConfidence: 0.7).count == 2)
        #expect(RoleBulk.acceptSuggestions(strips, minConfidence: 0.7, within: ["bus.1"]).map(\.strip) == ["bus.1"])
    }

    @Test func `batches and undo batches use the confirm_roles shape`() throws {
        let b = RoleBulk.batch(RoleBulk.lock(strips, ["bus.1"]) + RoleBulk.reset(strips, ["bus.9"]))
        #expect(b["bus.1"]?.string == "unknown" && b["bus.9"] == .null)
        let reply = try ControlClient.parseReply(#"{"ok":true,"result":{"key":"k","confirmed":1,"cleared":1,"previous":{"bus.1":null,"bus.9":"monitor"}}}"#)
        let u = try #require(RoleBulk.undoBatch(fromReply: reply))
        #expect(u["bus.1"] == .null && u["bus.9"]?.string == "monitor")
        #expect(RoleBulk.undoBatch(fromReply: .object(["previous": .object([:])])) == nil)
    }

    @Test func `role lists carry the console, scene and context flag`() throws {
        let raw = #"{"ok":true,"result":{"key":"sim|PUBLIC","console":{"kind":"sim","name":"wing-sim (in-process)","scene":"PUBLIC"},"strips":[{"strip":"aux.5","name":"B. LINK","detected":"monitor","effective":"monitor","confidence":1,"evidence":[],"confirmed":null,"needs_confirmation":false,"context":true}]}}"#
        let r = try ControlClient.parseReply(raw, as: RoleList.self)
        #expect(r.console?.scene == "PUBLIC" && r.strips[0].context == true)
        let c = try ControlClient.parseReply(
            #"{"ok":true,"result":{"consoles":[{"kind":"sim"},{"kind":"wing","ip":"127.0.0.1","port":2222,"name":"WINGSIM","model":"wing-sim","local":true,"scene":null}]}}"#,
            as: ConsoleList.self)
        #expect(c.consoles.map(\.id) == ["sim", "local"] && c.consoles[1].title.contains("this Mac"))
    }
}

@Suite struct UsbPatchTests {
    @Test func `patch labels and tap warnings`() throws {
        #expect(UsbPatch.describe(grp: "MAIN", input: 2) == "Main 1 R")
        #expect(UsbPatch.describe(grp: "MTX", input: 5) == "Matrix 3 L")
        #expect(UsbPatch.describe(grp: "MON", input: 1) == "Monitor A L (solo)")
        #expect(UsbPatch.paramIds([45]) == ["output.usb.45/grp", "output.usb.45/in"])
        let reply = try ControlClient.parseReply(#"{"ok":true,"result":{"output.usb.45/grp":{"type":"enum","value":"MTX"},"output.usb.45/in":{"type":"int","value":5},"output.usb.46/grp":{"type":"enum","value":"MTX"},"output.usb.46/in":{"type":"int","value":6},"output.usb.47/grp":{"type":"enum","value":"MAIN"},"output.usb.47/in":{"type":"int","value":1},"output.usb.48/grp":{"type":"enum","value":"MAIN"},"output.usb.48/in":{"type":"int","value":2}}}"#)
        let p = UsbPatch.parse(reply, channels: [45, 46, 47, 48])
        #expect(p[45]?.grp == "MTX" && p[45]?.input == 5)
        #expect(UsbPatch.warning(tap: "main", channels: [45, 46], patch: p)?.contains("Matrix 3 L") == true)
        #expect(UsbPatch.warning(tap: "main", channels: [47, 48], patch: p) == nil)
        #expect(UsbPatch.warning(tap: "solo", channels: [47, 48], patch: p) != nil)
        #expect(UsbPatch.warning(tap: "input.3", channels: [45], patch: p) == nil)
        #expect(UsbPatch.warning(tap: "main", channels: [1, 2], patch: p) == nil, "unknown patch: no warning")
    }
}

@Suite struct ToolPathTests {
    @Test func `app-launched daemon gets the usual tool dirs on PATH`() {
        let env = DaemonProcess.environment(base: ["PATH": "/usr/bin:/bin", "X": "1"], home: "/Users/u")
        let dirs = env["PATH"]!.split(separator: ":").map(String.init)
        #expect(dirs.prefix(2) == ["/usr/bin", "/bin"])
        #expect(dirs.contains("/Users/u/.local/bin") && dirs.contains("/opt/homebrew/bin"))
        #expect(env["X"] == "1")
        let again = DaemonProcess.environment(base: env, home: "/Users/u")
        #expect(again["PATH"] == env["PATH"], "idempotent")
        let s = try? DaemonStatus.decode(Data(#"{"running":true,"pid":1,"updated_unix":0,"tools":{"uv":{"path":null,"for":"stems"},"claude":{"path":"/x/claude","for":"llm"}}}"#.utf8))
        #expect(s?.missingTool("uv")?.for == "stems" && s?.missingTool("claude") == nil)
    }
}
