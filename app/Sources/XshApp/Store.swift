import AppKit
import Combine

/// Runs the bundled `xsh` binary. All settings logic lives there.
enum XshCLI {
    struct Result {
        let status: Int32
        let out: Data
        let err: String
        var text: String { String(decoding: out, as: UTF8.self) }
    }

    static let binary: URL = {
        if let p = ProcessInfo.processInfo.environment["XSH_BIN"], !p.isEmpty {
            return URL(fileURLWithPath: p)
        }
        return Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/xsh")
    }()

    static func run(_ args: [String], stdin: Data? = nil, timeout: TimeInterval = 30) -> Result {
        let p = Process()
        p.executableURL = binary
        p.arguments = args
        let (out, err) = (Pipe(), Pipe())
        let input = stdin == nil ? nil : Pipe()
        p.standardOutput = out
        p.standardError = err
        p.standardInput = input ?? FileHandle.nullDevice
        do {
            try p.run()
        } catch {
            return Result(status: 127, out: Data(), err: "cannot start \(binary.path): \(error.localizedDescription)")
        }
        if let stdin, let input {
            // A child that exits early must give EPIPE, not a SIGPIPE that ends
            // the app; a child that writes first must not block the caller.
            let h = input.fileHandleForWriting
            _ = fcntl(h.fileDescriptor, F_SETNOSIGPIPE, 1)
            DispatchQueue.global().async {
                try? h.write(contentsOf: stdin)
                try? h.close()
            }
        }
        // Read both pipes at the same time: a full pipe blocks the child.
        var outData = Data()
        var errData = Data()
        let done = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            let group = DispatchGroup()
            group.enter()
            DispatchQueue.global().async {
                errData = err.fileHandleForReading.readDataToEndOfFile()
                group.leave()
            }
            outData = out.fileHandleForReading.readDataToEndOfFile()
            group.wait()
            p.waitUntilExit()
            done.signal()
        }
        if done.wait(timeout: .now() + timeout) == .timedOut {
            if p.isRunning { p.terminate() }
            DispatchQueue.global().asyncAfter(deadline: .now() + 0.5) {
                if p.isRunning { kill(p.processIdentifier, SIGKILL) }
            }
            return Result(status: 124, out: Data(), err: "command timed out: xsh \(args.joined(separator: " "))")
        }
        return Result(status: p.terminationStatus, out: outData, err: String(decoding: errData, as: UTF8.self))
    }
}

extension Notification.Name {
    static let xshSettingsChanged = Notification.Name("xshSettingsChanged")
}

/// The settings, as `xsh app state` reports them. Keys are `section.key`
/// for `config.toml` and `ai.section.key` for `ai.toml`.
final class Store: ObservableObject {
    static let shared = Store()

    @Published private(set) var root: [String: Any] = [:]
    @Published var error: String?

    private let writer = DispatchQueue(label: "xsh.settings.writer")
    private var pending: [String: (work: DispatchWorkItem, value: Any)] = [:]
    private var inFlight: [String: (id: UUID, value: Any)] = [:]
    private var flat: [String: Any] = [:]
    private var lastOut = Data()
    private var watchedFiles: [String] = []
    private var stamp = ""
    private var timer: Timer?

    private init() {}

    // MARK: Read

    var config: [String: Any] { root["config"] as? [String: Any] ?? [:] }
    var ai: [String: Any] { root["ai"] as? [String: Any] ?? [:] }
    var paths: [String: Any] { root["paths"] as? [String: Any] ?? [:] }
    var themes: [[String: Any]] { root["themes"] as? [[String: Any]] ?? [] }
    var bundles: [[String: Any]] { root["bundles"] as? [[String: Any]] ?? [] }
    var profiles: [[String: Any]] { config["profiles"] as? [[String: Any]] ?? [] }
    var allKeys: [String] { root["keys"] as? [String] ?? [] }
    var version: String { root["version"] as? String ?? "" }
    var saveError: String? { error ?? (root["config_error"] as? String) }

    func path(_ name: String) -> String { paths[name] as? String ?? "" }

    func value(_ key: String) -> Any? { flat[key] }

    func bool(_ key: String) -> Bool { value(key) as? Bool ?? false }
    func int(_ key: String) -> Int { (value(key) as? NSNumber)?.intValue ?? 0 }
    func double(_ key: String) -> Double { (value(key) as? NSNumber)?.doubleValue ?? 0 }
    func string(_ key: String) -> String { value(key) as? String ?? "" }
    func strings(_ key: String) -> [String] { value(key) as? [String] ?? [] }

    // MARK: Load

    func start() {
        reloadNow()
        let files = currentWatchFiles()
        writer.async { [weak self] in
            self?.watchedFiles = files
            self?.stamp = Self.fileStamp(for: files)
        }
        let t = Timer(timeInterval: 1, repeats: true) { [weak self] _ in self?.poll() }
        t.tolerance = 0.3
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func reloadNow() {
        apply(XshCLI.run(["app", "state"]))
    }

    func reload() {
        writer.async { [weak self] in
            let r = XshCLI.run(["app", "state"])
            DispatchQueue.main.async { self?.apply(r) }
        }
    }

    private static func flatten(_ root: [String: Any]) -> [String: Any] {
        var out: [String: Any] = [:]
        func walk(_ dict: [String: Any], prefix: String) {
            for (k, v) in dict {
                let full = prefix.isEmpty ? k : "\(prefix).\(k)"
                out[full] = v
                if let nested = v as? [String: Any] {
                    walk(nested, prefix: full)
                }
            }
        }
        if let cfg = root["config"] as? [String: Any] { walk(cfg, prefix: "") }
        if let ai = root["ai"] as? [String: Any] { walk(ai, prefix: "ai") }
        return out
    }

    private func apply(_ r: XshCLI.Result) {
        if error == nil, r.status == 0, r.out == lastOut, pending.isEmpty, inFlight.isEmpty {
            return
        }
        guard r.status == 0,
              var next = (try? JSONSerialization.jsonObject(with: r.out)) as? [String: Any]
        else {
            error = r.err.isEmpty ? "`xsh app state` failed" : r.err.trimmingCharacters(in: .whitespacesAndNewlines)
            return
        }
        lastOut = r.out
        // A value still waiting to be saved wins over the older value on disk.
        for (key, f) in inFlight { Self.put(&next, key, f.value) }
        for (key, p) in pending { Self.put(&next, key, p.value) }
        root = next
        flat = Self.flatten(next)
        let files = currentWatchFiles()
        writer.async { [weak self] in self?.watchedFiles = files }
        NotificationCenter.default.post(name: .xshSettingsChanged, object: nil)
    }

    private func currentWatchFiles() -> [String] {
        ["config_file", "ai_file", "themes_dir"].map { path($0) } + [path("config_dir") + "/bundles"]
    }

    private static func fileStamp(for files: [String]) -> String {
        let fm = FileManager.default
        return files.map { f in
            let d = (try? fm.attributesOfItem(atPath: f))?[.modificationDate] as? Date
            return String(d?.timeIntervalSince1970 ?? 0)
        }.joined(separator: "|")
    }

    private func poll() {
        writer.async { [weak self] in
            guard let self, !self.watchedFiles.isEmpty else { return }
            let now = Self.fileStamp(for: self.watchedFiles)
            guard now != self.stamp else { return }
            self.stamp = now
            let r = XshCLI.run(["app", "state"])
            DispatchQueue.main.async { self.apply(r) }
        }
    }

    // MARK: Write

    private static func put(_ root: inout [String: Any], _ key: String, _ value: Any) {
        var parts = key.split(separator: ".").map(String.init)
        guard !parts.isEmpty else { return }
        let top = parts.first == "ai" ? parts.removeFirst() : "config"
        func set(_ d: [String: Any], _ path: ArraySlice<String>) -> [String: Any] {
            var d = d
            guard let head = path.first else { return d }
            if path.count == 1 {
                d[head] = value
            } else {
                d[head] = set(d[head] as? [String: Any] ?? [:], path.dropFirst())
            }
            return d
        }
        root[top] = set(root[top] as? [String: Any] ?? [:], parts[...])
    }

    private static func toml(_ v: Any) -> String {
        if let b = v as? Bool { return b ? "true" : "false" }
        if let n = v as? NSNumber { return n.stringValue }
        let opts: JSONSerialization.WritingOptions = [.fragmentsAllowed, .withoutEscapingSlashes]
        guard let d = try? JSONSerialization.data(withJSONObject: v, options: opts) else { return "\"\"" }
        return String(decoding: d, as: UTF8.self)
    }

    /// Changes the value now and saves it after a short pause, so a slider
    /// or a typed word gives one write and not one per step.
    func set(_ key: String, _ value: Any) {
        Self.put(&root, key, value)
        flat[key] = value
        lastOut = Data()
        NotificationCenter.default.post(name: .xshSettingsChanged, object: nil)
        pending[key]?.work.cancel()
        let isAI = key.hasPrefix("ai.")
        let name = isAI ? String(key.dropFirst(3)) : key
        // Blank lines of a list are only there while the user types.
        let raw = Self.toml((value as? [String])?.filter { !$0.isEmpty } ?? value)
        // In `ai.toml`, an empty text means "use the default", which is no entry.
        let args = isAI
            ? (value as? String == "" ? ["app", "ai-unset", name] : ["app", "ai-set", name, raw])
            : ["config", "set", name, raw]
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.pending[key] = nil
            let token = UUID()
            self.inFlight[key] = (token, value)
            self.save(args) { _ in
                if self.inFlight[key]?.id == token { self.inFlight[key] = nil }
            }
        }
        pending[key] = (work, value)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25, execute: work)
    }

    /// Goes back to the default value.
    func unset(_ key: String) {
        pending[key]?.work.cancel()
        pending[key] = nil
        let isAI = key.hasPrefix("ai.")
        save(isAI ? ["app", "ai-unset", String(key.dropFirst(3))] : ["config", "unset", key])
    }

    func save(_ args: [String], stdin: Data? = nil, done: ((XshCLI.Result) -> Void)? = nil) {
        writer.async { [weak self] in
            let r = XshCLI.run(args, stdin: stdin)
            let state = XshCLI.run(["app", "state"])
            if let self { self.stamp = Self.fileStamp(for: self.watchedFiles) }
            DispatchQueue.main.async {
                guard let self else { return }
                self.error = r.status == 0 ? nil : r.err.trimmingCharacters(in: .whitespacesAndNewlines)
                self.apply(state)
                done?(r)
            }
        }
    }

    func saveProfiles(_ list: [[String: Any]]) {
        guard let d = try? JSONSerialization.data(withJSONObject: list) else { return }
        save(["app", "set-profiles"], stdin: d)
    }
}
