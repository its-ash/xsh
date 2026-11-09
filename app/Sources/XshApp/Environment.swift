import AppKit
import Darwin

/// Facts about the user's session and about running processes.
enum Session {
    /// The user's login shell from the account database.
    static let loginShell: String = {
        if let e = getpwuid(getuid()), let s = e.pointee.pw_shell, let v = String(validatingUTF8: s), !v.isEmpty {
            return v
        }
        return "/bin/zsh"
    }()

    /// `PATH` of a login shell. An app that starts from Finder only gets a
    /// short system `PATH`, so Homebrew and similar tools would be missing.
    static let loginPath: String = {
        let fallback = ProcessInfo.processInfo.environment["PATH"] ?? "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        let p = Process()
        p.executableURL = URL(fileURLWithPath: loginShell)
        p.arguments = ["-lc", "printf %s \"$PATH\""]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        p.standardInput = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return fallback }
        // A slow profile must not hold the first window for long.
        DispatchQueue.global().asyncAfter(deadline: .now() + 2) { if p.isRunning { p.terminate() } }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        let text = String(decoding: data, as: UTF8.self)
        return p.terminationStatus == 0 && !text.isEmpty ? text : fallback
    }()

    /// Only a program with this token can ask for an agent pane. Text that a
    /// program prints, such as a file or the output of `ssh`, does not have it.
    static let agentToken = UUID().uuidString

    /// The environment of a new session.
    static func environment(extra: [String]) -> [String] {
        var env = ProcessInfo.processInfo.environment
        env.removeValue(forKey: "XSH_BIN")
        let term = Store.shared.string("terminal.term")
        env["TERM"] = term.isEmpty ? "xterm-256color" : term
        env["COLORTERM"] = "truecolor"
        env["TERM_PROGRAM"] = "xsh"
        env["TERM_PROGRAM_VERSION"] = Store.shared.version
        env["SHELL"] = env["SHELL"] ?? loginShell
        if env["LANG"] == nil { env["LANG"] = Locale.current.identifier + ".UTF-8" }
        // The `xsh` of this app comes first: an older copy can be on the `PATH`.
        env["PATH"] = XshCLI.binary.deletingLastPathComponent().path + ":" + loginPath
        for pair in Store.shared.strings("terminal.env") + extra {
            guard let eq = pair.firstIndex(of: "="), eq != pair.startIndex else { continue }
            env[String(pair[..<eq])] = String(pair[pair.index(after: eq)...])
        }
        env["XSH_APP_TOKEN"] = agentToken
        return env.map { "\($0.key)=\($0.value)" }
    }

    /// Whether `pid` has child processes: a program runs in the session.
    static func hasChildren(_ pid: pid_t) -> Bool {
        guard pid > 0 else { return false }
        var ids = [pid_t](repeating: 0, count: 64)
        let bytes = ids.withUnsafeMutableBytes { buf in
            proc_listpids(UInt32(PROC_PPID_ONLY), UInt32(pid), buf.baseAddress, Int32(buf.count))
        }
        return bytes > 0 && ids.prefix(Int(bytes) / MemoryLayout<pid_t>.size).contains { $0 > 0 }
    }

    /// The working folder of `pid`.
    static func cwd(_ pid: pid_t) -> String? {
        guard pid > 0 else { return nil }
        var info = proc_vnodepathinfo()
        let size = Int32(MemoryLayout<proc_vnodepathinfo>.size)
        guard proc_pidinfo(pid, PROC_PIDVNODEPATHINFO, 0, &info, size) == size else { return nil }
        return withUnsafePointer(to: &info.pvi_cdir.vip_path) { p in
            p.withMemoryRebound(to: CChar.self, capacity: Int(MAXPATHLEN)) { String(cString: $0) }
        }
    }
}
