import Foundation

/// Starting wing-mixd.
public enum DaemonProcess {
    public static func arguments(paths: DaemonPaths) -> [String] {
        ["--state-dir", paths.stateDir.path(percentEncoded: false)]
    }

    /// Usual tool locations (uv, claude, Homebrew) an app-launched process lacks: launchd gives GUI apps
    /// `/usr/bin:/bin:/usr/sbin:/sbin` only. The daemon also merges its login shell's PATH (wing-mixd `tools`).
    public static let extraPathDirs = [".local/bin", ".cargo/bin", "/opt/homebrew/bin", "/usr/local/bin"]

    /// The daemon's environment: `base` with `PATH` extended by `extraPathDirs` (missing entries appended).
    public static func environment(base: [String: String] = ProcessInfo.processInfo.environment,
                                   home: String = NSHomeDirectory()) -> [String: String] {
        var env = base
        var dirs = (base["PATH"] ?? "/usr/bin:/bin:/usr/sbin:/sbin").split(separator: ":").map(String.init)
        for d in extraPathDirs {
            let full = d.hasPrefix("/") ? d : (home as NSString).appendingPathComponent(d)
            if !dirs.contains(full) { dirs.append(full) }
        }
        env["PATH"] = dirs.joined(separator: ":")
        return env
    }

    /// Launch `binary` with its state dir, stdout/stderr appended to the log file.
    public static func launch(binary: URL, paths: DaemonPaths) throws -> Process {
        let fm = FileManager.default
        try fm.createDirectory(at: paths.stateDir, withIntermediateDirectories: true)
        let logPath = paths.logURL.path(percentEncoded: false)
        if !fm.fileExists(atPath: logPath) {
            fm.createFile(atPath: logPath, contents: nil)
        }
        let log = try FileHandle(forWritingTo: paths.logURL)
        try log.seekToEnd()
        let p = Process()
        p.executableURL = binary
        p.arguments = arguments(paths: paths)
        p.environment = environment()
        p.standardOutput = log
        p.standardError = log
        p.standardInput = FileHandle.nullDevice
        try p.run()
        return p
    }

    /// Last `maxLines` lines of the daemon log (for error reports).
    public static func logTail(paths: DaemonPaths, maxLines: Int = 5) -> String {
        guard let data = try? Data(contentsOf: paths.logURL) else { return "" }
        let lines = String(decoding: data.suffix(16_384), as: UTF8.self)
            .split(separator: "\n").suffix(maxLines)
        return lines.joined(separator: "\n")
    }

    /// Wait (blocking, up to `timeout`) until the daemon answers `ping`.
    public static func waitUntilReady(paths: DaemonPaths, timeout: TimeInterval = 20) -> Bool {
        let client = ControlClient(socketURL: paths.socketURL, timeout: 2)
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if (try? client.sendLine("ping")) != nil { return true }
            Thread.sleep(forTimeInterval: 0.1)
        }
        return false
    }
}
