import Foundation

/// Where the daemon keeps its state and where its binary is.
public struct DaemonPaths: Sendable, Equatable {
    public static let statusFile = "status.json"
    public static let socketFile = "control.sock"
    public static let logFile = "wing-mixd.log"
    /// Info.plist key build.sh sets to the repo root (fallback binary location).
    public static let repoRootKey = "WingMixRepoRoot"

    public var stateDir: URL

    public init(stateDir: URL) {
        self.stateDir = stateDir
    }

    /// `$WINGMIX_STATE_DIR`, else `~/Library/Application Support/WingMix` (the daemon's default
    /// `--state-dir`; never the WingSimulator state dir).
    public static func standard(environment: [String: String] = ProcessInfo.processInfo.environment)
        -> DaemonPaths
    {
        if let d = environment["WINGMIX_STATE_DIR"], !d.isEmpty {
            return DaemonPaths(stateDir: URL(filePath: d, directoryHint: .isDirectory))
        }
        let home = FileManager.default.homeDirectoryForCurrentUser
        return DaemonPaths(stateDir: home.appending(path: "Library/Application Support/WingMix",
                                                    directoryHint: .isDirectory))
    }

    public var statusURL: URL { stateDir.appending(path: Self.statusFile) }
    public var socketURL: URL { stateDir.appending(path: Self.socketFile) }
    public var logURL: URL { stateDir.appending(path: Self.logFile) }
}

/// Finds the `wing-mixd` executable.
public struct BinaryLocator: Sendable {
    public var searchDirs: [URL]

    public init(searchDirs: [URL]) {
        self.searchDirs = searchDirs
    }

    /// `$WINGMIX_BIN_DIR`, the app bundle's `Contents/Resources/bin`, then `<repo>/target/release`
    /// from the bundle's `WingMixRepoRoot` key.
    public static func standard(bundle: Bundle = .main,
                                environment: [String: String] = ProcessInfo.processInfo.environment)
        -> BinaryLocator
    {
        var dirs: [URL] = []
        if let d = environment["WINGMIX_BIN_DIR"], !d.isEmpty {
            dirs.append(URL(filePath: d, directoryHint: .isDirectory))
        }
        if let r = bundle.resourceURL {
            dirs.append(r.appending(path: "bin", directoryHint: .isDirectory))
        }
        if let root = bundle.object(forInfoDictionaryKey: DaemonPaths.repoRootKey) as? String, !root.isEmpty {
            dirs.append(URL(filePath: root, directoryHint: .isDirectory)
                .appending(path: "target/release", directoryHint: .isDirectory))
        }
        return BinaryLocator(searchDirs: dirs)
    }

    /// First executable named `name`.
    public func find(_ name: String) -> URL? {
        searchDirs.lazy
            .map { $0.appending(path: name) }
            .first { FileManager.default.isExecutableFile(atPath: $0.path(percentEncoded: false)) }
    }
}
