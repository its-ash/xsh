import AppKit
import UniformTypeIdentifiers

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    private var settingsWindow: NSWindow?
    private var pendingURLs: [URL] = []
    private var launched = false
    private var menuStamp = ""

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSWindow.allowsAutomaticWindowTabbing = false
        Store.shared.start()
        _ = Session.loginPath
        NSApp.servicesProvider = self
        rebuildMenu()
        applyAppWide()
        NotificationCenter.default.addObserver(
            forName: .xshSettingsChanged, object: nil, queue: .main
        ) { [weak self] _ in
            self?.rebuildMenu()
            self?.applyAppWide()
        }
        // SwiftTerm has no selection callback, so check after each mouse release.
        NSEvent.addLocalMonitorForEvents(matching: .leftMouseUp) { event in
            DispatchQueue.main.async {
                guard Store.shared.bool("terminal.copy_on_select"),
                      let s = NSApp.keyWindow?.firstResponder as? Surface, s.selectionActive
                else { return }
                s.copy(self)
            }
            return event
        }
        if !Terminals.restore(), pendingURLs.isEmpty {
            Terminals.newWindow(profile: nil)
        }
        launched = true
        handle(pendingURLs)
        pendingURLs = []
        Updater.checkAtLaunch()
    }

    func applicationWillTerminate(_ notification: Notification) {
        Terminals.save()
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if Updater.installing { return .terminateNow }
        let s = Store.shared.string("window.confirm_close")
        let busy = TerminalWindow.all.filter(\.isBusy).count
        guard s == "always" || (s == "processes" && busy > 0) else { return .terminateNow }
        let a = NSAlert()
        a.messageText = "Quit xsh?"
        a.informativeText = busy > 0 ? "Programs still run in \(busy) tab(s). Quitting stops them." : ""
        a.addButton(withTitle: "Quit")
        a.addButton(withTitle: "Cancel")
        return a.runModal() == .alertFirstButtonReturn ? .terminateNow : .terminateCancel
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        if !hasVisibleWindows { Terminals.newWindow(profile: nil) }
        return true
    }

    private func applyAppWide() {
        switch Store.shared.string("window.appearance") {
        case "light": NSApp.appearance = NSAppearance(named: .aqua)
        case "dark": NSApp.appearance = NSAppearance(named: .darkAqua)
        default: NSApp.appearance = nil
        }
        GlobalHotkey.update()
    }

    // MARK: Files and links from other apps

    func application(_ application: NSApplication, open urls: [URL]) {
        if launched { handle(urls) } else { pendingURLs += urls }
    }

    private func handle(_ urls: [URL]) {
        for u in urls {
            if u.scheme == "ssh" {
                openSSH(u)
            } else if u.isFileURL {
                var isDir: ObjCBool = false
                guard FileManager.default.fileExists(atPath: u.path, isDirectory: &isDir) else { continue }
                if isDir.boolValue {
                    Terminals.newTab(profile: nil, directory: u.path)
                } else {
                    let w = Terminals.newTab(profile: nil, directory: u.deletingLastPathComponent().path)
                    runSoon(in: w, shellQuote(u.path))
                }
            }
        }
    }

    /// A link can come from any web page: check it and ask first.
    private func openSSH(_ u: URL) {
        let safe = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-:")
        let ok = { (s: String) in !s.hasPrefix("-") && s.unicodeScalars.allSatisfy(safe.contains) }
        guard let host = u.host, !host.isEmpty, ok(host), ok(u.user ?? "") else { return }
        let target = (u.user.map { $0 + "@" } ?? "") + host
        var cmd = "ssh"
        if let port = u.port { cmd += " -p \(port)" }
        cmd += " -- " + shellQuote(target)
        let a = NSAlert()
        a.messageText = "Open an SSH connection to \(target)?"
        a.informativeText = "Another app asked xsh to run:\n\(cmd)"
        a.addButton(withTitle: "Connect")
        a.addButton(withTitle: "Cancel")
        guard a.runModal() == .alertFirstButtonReturn else { return }
        runSoon(in: Terminals.newTab(profile: nil), cmd)
    }

    /// The shell needs a moment to start before it can read a line.
    private func runSoon(in w: TerminalWindow, _ line: String) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { w.type(line) }
    }

    /// Finder service: `New xsh Tab Here`.
    @objc func openFolderService(_ pb: NSPasteboard, userData: String, error: AutoreleasingUnsafeMutablePointer<NSString>) {
        let opts: [NSPasteboard.ReadingOptionKey: Any] = [.urlReadingFileURLsOnly: true]
        let urls = pb.readObjects(forClasses: [NSURL.self], options: opts) as? [URL] ?? []
        NSApp.activate(ignoringOtherApps: true)
        handle(urls)
    }

    // MARK: Menu

    private func rebuildMenu() {
        let s = Store.shared
        let names = s.profiles.compactMap { $0["name"] as? String }
        let keys = s.config["keys"] as? [String: Any] ?? [:]
        let stamp = names.joined(separator: "\u{1}") + keys.map { "\($0.key)=\($0.value)" }.sorted().joined()
        guard stamp != menuStamp else { return }
        menuStamp = stamp
        NSApp.mainMenu = MainMenu.build(delegate: self, profiles: names)
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool { true }

    @objc func newWindow(_ sender: Any?) { Terminals.newWindow(profile: nil) }
    @objc func newTab(_ sender: Any?) { Terminals.newTab(profile: nil) }

    private func profile(_ sender: Any?) -> Profile? {
        guard let name = (sender as? NSMenuItem)?.representedObject as? String else { return nil }
        return Store.shared.profiles.map(Profile.init).first { $0.name == name }
    }

    @objc func newTabWithProfile(_ sender: Any?) { Terminals.newTab(profile: profile(sender)) }
    @objc func newWindowWithProfile(_ sender: Any?) { Terminals.newWindow(profile: profile(sender)) }

    @objc func checkForUpdates(_ sender: Any?) { Updater.check(manual: true) }

    @objc func showHelp(_ sender: Any?) {
        if let u = URL(string: "https://github.com/its-ash/rushy#readme") { NSWorkspace.shared.open(u) }
    }

    @objc func showSettings(_ sender: Any?) {
        if settingsWindow == nil {
            let w = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 860, height: 620),
                styleMask: [.titled, .closable, .miniaturizable, .resizable],
                backing: .buffered,
                defer: false)
            w.title = "xsh Settings"
            w.contentViewController = SettingsHost()
            w.setContentSize(NSSize(width: 860, height: 620))
            w.minSize = NSSize(width: 680, height: 420)
            w.isReleasedWhenClosed = false
            w.tabbingMode = .disallowed
            w.center()
            settingsWindow = w
        }
        settingsWindow?.makeKeyAndOrderFront(sender)
        NSApp.activate(ignoringOtherApps: true)
    }
}
