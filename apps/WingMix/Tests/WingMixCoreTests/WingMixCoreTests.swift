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
        let consoles = try await client.call(ConsoleList.self, "list_consoles", ["discover": false])
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

        // no session yet: STOP and PANIC are refused with a message, not a hang
        await #expect(throws: ControlError.daemon("no session: nothing to stop")) { try await client.request("STOP") }
        _ = try await client.request("quit")
        proc.waitUntilExit()
        #expect(!FileManager.default.fileExists(atPath: paths.socketURL.path(percentEncoded: false)))
    }
}
