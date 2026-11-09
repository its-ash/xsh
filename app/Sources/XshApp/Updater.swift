import AppKit

/// Finds and installs new versions. The `xsh` binary does the network work and
/// checks each download, and this code replaces the app after the user agrees.
enum Updater {
    private static let lastCheckKey = "xsh.lastUpdateCheck"
    private static let checkInterval: TimeInterval = 24 * 60 * 60

    /// While true, quitting asks no question: the user already agreed.
    private(set) static var installing = false

    private static var bundlePath: String { Bundle.main.bundlePath }
    private static var isDevBuild: Bool { bundlePath.hasSuffix("/target/xsh.app") }

    static func checkAtLaunch() {
        let d = UserDefaults.standard
        let last = d.object(forKey: lastCheckKey) as? Date ?? .distantPast
        guard Store.shared.bool("updates.auto_check"), !isDevBuild,
              Date().timeIntervalSince(last) >= checkInterval
        else { return }
        d.set(Date(), forKey: lastCheckKey)
        check(manual: false)
    }

    static func check(manual: Bool) {
        DispatchQueue.global().async {
            let r = XshCLI.run(["app", "update-check"])
            let v = (try? JSONSerialization.jsonObject(with: r.out)) as? [String: Any]
            DispatchQueue.main.async {
                guard r.status == 0, let v else {
                    if manual { show("Cannot check for updates", r.err) }
                    return
                }
                if v["update"] as? Bool == true, let version = v["latest"] as? String {
                    offer(version, notes: v["notes"] as? String ?? "")
                } else if manual {
                    show("xsh is up to date", "You have version \(v["current"] as? String ?? "").")
                }
            }
        }
    }

    private static func show(_ title: String, _ text: String) {
        let a = NSAlert()
        a.messageText = title
        a.informativeText = text.trimmingCharacters(in: .whitespacesAndNewlines)
        a.runModal()
    }

    private static func offer(_ version: String, notes: String) {
        guard !installing else { return }
        let a = NSAlert()
        a.messageText = "xsh \(version) is available"
        a.informativeText = "xsh restarts to install it. Programs that run in xsh stop.\n\n" + notes
        a.addButton(withTitle: "Install and Restart")
        a.addButton(withTitle: "Later")
        guard a.runModal() == .alertFirstButtonReturn else { return }
        install(version)
    }

    private static func install(_ version: String) {
        let parent = (bundlePath as NSString).deletingLastPathComponent
        guard FileManager.default.isWritableFile(atPath: parent) else {
            show("Cannot install the update", "xsh cannot write to \(parent). Move xsh.app to a folder you can change.")
            return
        }
        NSApp.dockTile.badgeLabel = "↓"
        DispatchQueue.global().async {
            let r = XshCLI.run(["app", "update-download", version])
            let v = (try? JSONSerialization.jsonObject(with: r.out)) as? [String: Any]
            DispatchQueue.main.async {
                NSApp.dockTile.badgeLabel = nil
                guard r.status == 0, let path = v?["app"] as? String else {
                    show("Cannot install the update", r.err)
                    return
                }
                replaceAndRestart(with: path)
            }
        }
    }

    /// A small helper waits for this process to end, swaps the apps, and starts
    /// the result. If the swap fails, the old app comes back.
    private static func replaceAndRestart(with newApp: String) {
        let script = """
        while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
        old="$3.old.$$"
        if mv "$3" "$old"; then
            if mv "$2" "$3"; then rm -rf "$old"; else mv "$old" "$3"; fi
        fi
        open "$3"
        """
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", script, "xsh-update", String(getpid()), newApp, bundlePath]
        p.standardInput = FileHandle.nullDevice
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        do {
            try p.run()
        } catch {
            show("Cannot install the update", error.localizedDescription)
            return
        }
        installing = true
        NSApp.terminate(nil)
    }
}
