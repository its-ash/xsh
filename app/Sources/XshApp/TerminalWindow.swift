import AppKit

/// A thin divider between two panes.
final class Split: NSSplitView {
    override var dividerThickness: CGFloat { 1 }
    override var dividerColor: NSColor { .separatorColor }

    init(vertical: Bool, frame: NSRect) {
        super.init(frame: frame)
        isVertical = vertical
        dividerStyle = .thin
        autoresizingMask = [.width, .height]
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func applyRatio(_ ratio: Double) {
        let total = isVertical ? bounds.width : bounds.height
        let clamped = min(max(ratio, 0.1), 0.9)
        setPosition((total - dividerThickness) * clamped, ofDividerAt: 0)
    }

    var currentRatio: Double {
        let total = isVertical ? bounds.width : bounds.height
        guard total > dividerThickness, let first = subviews.first else { return 0.5 }
        let part = isVertical ? first.frame.width : first.frame.height
        return Double(min(max(part / (total - dividerThickness), 0.1), 0.9))
    }

    func equalize() {
        let total = isVertical ? bounds.width : bounds.height
        setPosition((total - dividerThickness) / 2, ofDividerAt: 0)
        subviews.compactMap { $0 as? Split }.forEach { $0.equalize() }
    }
}

enum FocusDirection { case left, right, up, down }

/// A saved pane layout.
indirect enum Layout {
    case pane(cwd: String?, profile: String?)
    case split(vertical: Bool, ratio: Double, Layout, Layout)

    var plist: [String: Any] {
        switch self {
        case let .pane(cwd, profile):
            return ["cwd": cwd ?? "", "profile": profile ?? ""]
        case let .split(v, r, a, b):
            return ["vertical": v, "ratio": r, "a": a.plist, "b": b.plist]
        }
    }

    init?(plist: [String: Any]) {
        if let v = plist["vertical"] as? Bool,
           let a = (plist["a"] as? [String: Any]).flatMap(Layout.init(plist:)),
           let b = (plist["b"] as? [String: Any]).flatMap(Layout.init(plist:)) {
            let r = (plist["ratio"] as? NSNumber)?.doubleValue ?? 0.5
            self = .split(vertical: v, ratio: r, a, b)
        } else if plist["cwd"] != nil {
            let (c, p) = (plist["cwd"] as? String ?? "", plist["profile"] as? String ?? "")
            self = .pane(cwd: c.isEmpty ? nil : c, profile: p.isEmpty ? nil : p)
        } else {
            return nil
        }
    }
}

/// Sends full screen (the menu item, its shortcut and the green button) to
/// xsh's own full screen; see `TerminalWindow.toggleScreenFill()`.
final class TerminalNSWindow: NSWindow {
    override func toggleFullScreen(_ sender: Any?) {
        guard let controller = windowController as? TerminalWindow else {
            super.toggleFullScreen(sender)
            return
        }
        controller.toggleScreenFill()
    }

    /// macOS keeps a titled window clear of the Dock and the menu bar. In
    /// full screen both are hidden, and the window must cover their space.
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect {
        if (windowController as? TerminalWindow)?.fillsScreen == true { return frameRect }
        return super.constrainFrameRect(frameRect, to: screen)
    }

    override func validateMenuItem(_ item: NSMenuItem) -> Bool {
        if item.action == #selector(toggleFullScreen(_:)), let controller = windowController as? TerminalWindow {
            item.title = controller.fillsScreen ? "Exit Full Screen" : "Enter Full Screen"
            return true
        }
        return super.validateMenuItem(item)
    }
}

/// One native window, which macOS shows as one tab of a tab group.
final class TerminalWindow: NSWindowController, NSWindowDelegate {
    static private(set) var all: [TerminalWindow] = []

    private let host = NSView()
    private let blur = NSVisualEffectView()
    private var observer: NSKeyValueObservation?
    private var settings: NSObjectProtocol?
    private var skipConfirm = false
    private(set) var active: Pane?
    /// The frame to go back to, while the window fills its screen.
    private var screenFillRestore: NSRect?

    /// Whether the window is in xsh's full screen.
    var fillsScreen: Bool { screenFillRestore != nil }

    private var panes: [Pane] {
        func walk(_ v: NSView) -> [Pane] {
            (v as? Pane).map { [$0] } ?? v.subviews.flatMap(walk)
        }
        return walk(host)
    }

    var isBusy: Bool { panes.contains(where: \.isBusy) }

    // MARK: Open

    init(layout: Layout, sizedFrom other: NSWindow? = nil) {
        let window = TerminalNSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 500),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false)
        super.init(window: window)
        window.delegate = self
        window.tabbingIdentifier = "xsh"
        window.title = "xsh"
        window.isRestorable = false
        window.isReleasedWhenClosed = false

        let root = NSView(frame: window.contentLayoutRect)
        root.autoresizingMask = [.width, .height]
        blur.frame = root.bounds
        blur.autoresizingMask = [.width, .height]
        blur.blendingMode = .behindWindow
        blur.material = .underWindowBackground
        blur.state = .active
        root.addSubview(blur)
        host.frame = root.bounds
        host.autoresizingMask = [.width, .height]
        root.addSubview(host)
        window.contentView = root

        if let other {
            window.setFrame(other.frame, display: false)
        } else {
            let s = Store.shared
            let font = Fonts.make(profile: nil)
            let cell = Fonts.cell(font, lineSpacing: CGFloat(s.int("font.line_spacing")) / 100)
            let pad = CGFloat(s.int("window.padding")) * 2
            window.setContentSize(NSSize(
                width: cell.width * CGFloat(max(s.int("window.columns"), 20)) + pad,
                height: cell.height * CGFloat(max(s.int("window.rows"), 5)) + pad))
        }

        applyWindowSettings()
        let first = build(layout, frame: hostFrame)
        first.autoresizingMask = [.width, .height]
        host.addSubview(first)
        applyRatios(layout, to: first)
        if other == nil, let pane = first as? Pane { fit(pane) }
        focus(panes.first)
        syncHeaders()

        observer = window.observe(\.firstResponder, options: [.new]) { [weak self] _, _ in
            self?.responderChanged()
        }
        settings = NotificationCenter.default.addObserver(
            forName: .xshSettingsChanged, object: nil, queue: .main
        ) { [weak self] _ in self?.reapply() }
        Self.all.append(self)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    deinit {
        if let settings { NotificationCenter.default.removeObserver(settings) }
    }

    /// Corrects the first estimate of the window size: the terminal knows its
    /// real cell size, and only reports it as columns and rows.
    private func fit(_ pane: Pane) {
        guard let window else { return }
        pane.layoutSubtreeIfNeeded()
        let s = Store.shared
        let (size, dims) = (pane.surface.frame.size, pane.surface.terminalDimensions)
        guard dims.cols > 0, dims.rows > 0 else { return }
        let want = NSSize(
            width: size.width / CGFloat(dims.cols) * CGFloat(max(s.int("window.columns"), 20)),
            height: size.height / CGFloat(dims.rows) * CGFloat(max(s.int("window.rows"), 5)))
        let content = window.contentLayoutRect.size
        window.setContentSize(NSSize(
            width: content.width + ceil(want.width - size.width),
            height: content.height + ceil(want.height - size.height)))
        host.frame = hostFrame
        pane.layoutSubtreeIfNeeded()
    }

    private func build(_ layout: Layout, frame: NSRect) -> NSView {
        switch layout {
        case let .pane(cwd, name):
            let p = name.flatMap { n in Store.shared.profiles.map(Profile.init).first { $0.name == n } }
            return makePane(profile: p, directory: cwd, frame: frame)
        case let .split(vertical, _, a, b):
            let s = Split(vertical: vertical, frame: frame)
            s.addSubview(build(a, frame: frame))
            s.addSubview(build(b, frame: frame))
            return s
        }
    }

    private func applyRatios(_ layout: Layout, to view: NSView) {
        guard case let .split(_, ratio, a, b) = layout, let s = view as? Split, s.subviews.count == 2 else { return }
        s.applyRatio(ratio)
        applyRatios(a, to: s.subviews[0])
        applyRatios(b, to: s.subviews[1])
    }

    private func makePane(profile: Profile?, directory: String?, frame: NSRect, agentSocket: String? = nil) -> Pane {
        let p = Pane(profile: profile, directory: directory, frame: frame, agentSocket: agentSocket)
        p.owner = self
        return p
    }

    func layoutSnapshot() -> Layout {
        func snap(_ v: NSView) -> Layout? {
            if let p = v as? Pane { return .pane(cwd: p.cwd, profile: p.profile?.name) }
            if let s = v as? Split, s.subviews.count == 2, let a = snap(s.subviews[0]), let b = snap(s.subviews[1]) {
                return .split(vertical: s.isVertical, ratio: s.currentRatio, a, b)
            }
            return v.subviews.lazy.compactMap(snap).first
        }
        return snap(host) ?? .pane(cwd: nil, profile: nil)
    }

    // MARK: Settings

    private var hostFrame: NSRect {
        guard let window, let root = window.contentView else { return .zero }
        // With a transparent title bar, the text stays below the traffic lights.
        return Store.shared.string("window.titlebar") == "transparent" || fillsScreen
            ? root.convert(window.contentLayoutRect, from: nil)
            : root.bounds
    }

    private func applyWindowSettings() {
        guard let window else { return }
        let s = Store.shared
        // Full screen keeps the window buttons, on a transparent bar.
        let bar = fillsScreen ? "transparent" : s.string("window.titlebar")
        window.isMovable = !fillsScreen
        window.titlebarAppearsTransparent = bar != "native"
        window.titleVisibility = bar == "hidden" || fillsScreen ? .hidden : .visible
        if bar == "native" {
            window.styleMask.remove(.fullSizeContentView)
        } else {
            window.styleMask.insert(.fullSizeContentView)
        }
        for b in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            window.standardWindowButton(b)?.isHidden = bar == "hidden"
        }
        window.level = s.bool("window.always_on_top") ? .floating : .normal

        let opacity = CGFloat(min(max(s.int("window.opacity"), 10), 100)) / 100
        let bg = Palette.current(profile: nil).background
        let useBlur = s.bool("window.blur") && opacity < 1
        blur.isHidden = !useBlur
        window.isOpaque = opacity >= 1
        window.backgroundColor = useBlur ? .clear : bg.withAlphaComponent(opacity)
        window.hasShadow = true
        host.frame = hostFrame
    }

    private func reapply() {
        applyWindowSettings()
        panes.forEach { $0.apply() }
        syncTabBar()
    }

    func syncTabBar() {
        guard let window, let group = window.tabGroup else { return }
        let always = Store.shared.string("window.tab_bar") == "always"
        if group.windows.count == 1, group.isTabBarVisible != always {
            window.toggleTabBar(nil)
        }
    }

    // MARK: Focus

    private func responderChanged() {
        guard let view = window?.firstResponder as? NSView else { return }
        if let pane = panes.first(where: { view === $0.surface || view.isDescendant(of: $0.surface) }) {
            setActive(pane)
        }
    }

    private func setActive(_ pane: Pane) {
        active = pane
        panes.forEach { $0.focused = $0 === pane }
        updateTitle()
    }

    func focus(_ pane: Pane?) {
        guard let pane else { return }
        setActive(pane)
        window?.makeFirstResponder(pane.surface)
    }

    func updateTitle() {
        guard let window, let pane = active else { return }
        let dir = pane.cwd
        let title = pane.title.isEmpty
            ? (dir.map { ($0 as NSString).abbreviatingWithTildeInPath } ?? "xsh")
            : pane.title
        if window.title != title {
            window.title = title
            window.tab.title = title
        }
        let url = dir.map { URL(fileURLWithPath: $0) }
        if window.representedURL != url {
            window.representedURL = url
        }
    }

    @objc func focusNextPane(_ sender: Any?) { cycle(1) }
    @objc func focusPreviousPane(_ sender: Any?) { cycle(-1) }
    @objc func focusLeft(_ sender: Any?) { move(.left) }
    @objc func focusRight(_ sender: Any?) { move(.right) }
    @objc func focusUp(_ sender: Any?) { move(.up) }
    @objc func focusDown(_ sender: Any?) { move(.down) }

    private func cycle(_ by: Int) {
        let list = panes
        guard let a = active, let i = list.firstIndex(of: a), !list.isEmpty else { return }
        focus(list[(i + by + list.count) % list.count])
    }

    private func move(_ d: FocusDirection) {
        guard let a = active else { return }
        let from = a.convert(a.bounds, to: nil)
        let candidates = panes.filter { $0 !== a }.map { ($0, $0.convert($0.bounds, to: nil)) }.filter { _, r in
            switch d {
            case .left: return r.maxX <= from.minX + 1 && r.maxY > from.minY && r.minY < from.maxY
            case .right: return r.minX >= from.maxX - 1 && r.maxY > from.minY && r.minY < from.maxY
            case .up: return r.minY >= from.maxY - 1 && r.maxX > from.minX && r.minX < from.maxX
            case .down: return r.maxY <= from.minY + 1 && r.maxX > from.minX && r.minX < from.maxX
            }
        }
        let gap = { (r: NSRect) -> CGFloat in
            switch d {
            case .left: return from.minX - r.maxX
            case .right: return r.minX - from.maxX
            case .up: return r.minY - from.maxY
            case .down: return from.minY - r.maxY
            }
        }
        focus(candidates.min { gap($0.1) < gap($1.1) }?.0)
    }

    // MARK: Split and close

    @objc func splitRight(_ sender: Any?) { split(active, vertical: true) }
    @objc func splitDown(_ sender: Any?) { split(active, vertical: false) }

    /// A pane to the right for the commands of the agent. The focus stays where
    /// it is, because the agent asks its questions in the left pane.
    func openAgentPane(beside pane: Pane, socket: String) {
        split(pane, vertical: true, agentSocket: socket)
    }

    private func split(_ target: Pane?, vertical: Bool, agentSocket: String? = nil) {
        guard let pane = target, let parent = pane.superview else { return }
        let current = active
        let frame = pane.frame
        let node = Split(vertical: vertical, frame: frame)
        node.autoresizingMask = pane.autoresizingMask
        parent.replaceSubview(pane, with: node)
        pane.frame = node.bounds
        node.addSubview(pane)
        let new = makePane(profile: pane.profile, directory: pane.cwd, frame: node.bounds, agentSocket: agentSocket)
        node.addSubview(new)
        node.adjustSubviews()
        node.equalize()
        syncHeaders()
        focus(agentSocket == nil ? new : current)
    }

    private func syncHeaders() {
        let list = panes
        list.forEach { $0.showsHeader = list.count > 1 }
    }

    @objc func equalizePanes(_ sender: Any?) {
        host.subviews.compactMap { $0 as? Split }.forEach { $0.equalize() }
    }

    /// Removes a pane. The window closes with its last pane.
    func remove(_ pane: Pane) {
        let others = panes.filter { $0 !== pane }
        guard let node = pane.superview as? Split,
              let sibling = node.subviews.first(where: { $0 !== pane }),
              let up = node.superview
        else {
            pane.shutdown()
            skipConfirm = true
            window?.close()
            return
        }
        // Read the size first: a split view that loses a pane can change it.
        let frame = node.frame
        let mask = node.autoresizingMask
        pane.shutdown()
        sibling.removeFromSuperview()
        up.replaceSubview(node, with: sibling)
        sibling.autoresizingMask = mask
        sibling.frame = up is NSSplitView ? frame : up.bounds
        // A split view takes a pane out of the autoresizing system. Outside a
        // split view, the pane has no constraints, so its next layout gives it
        // a size of zero, which is a blank window.
        if !(up is NSSplitView) { sibling.translatesAutoresizingMaskIntoConstraints = true }
        (up as? NSSplitView)?.adjustSubviews()
        sibling.needsLayout = true
        sibling.needsDisplay = true
        syncHeaders()
        // The move out of the window also takes the focus from the other pane.
        focus(active === pane ? others.first : active)
    }

    /// `Close`: the active pane, then the tab.
    @objc func closePane(_ sender: Any?) {
        guard let pane = active else { return }
        requestClose(pane)
    }

    /// Closes `pane` after the usual question. The last pane closes the tab.
    func requestClose(_ pane: Pane) {
        if panes.count == 1 {
            window?.performClose(nil)
            return
        }
        if confirm("Close this pane?", busy: pane.isBusy) { remove(pane) }
    }

    private func confirm(_ question: String, busy: Bool) -> Bool {
        let mode = Store.shared.string("window.confirm_close")
        guard mode == "always" || (mode == "processes" && busy) else { return true }
        let a = NSAlert()
        a.messageText = question
        a.informativeText = busy ? "A program still runs in it. Closing stops the program." : ""
        a.addButton(withTitle: "Close")
        a.addButton(withTitle: "Cancel")
        return a.runModal() == .alertFirstButtonReturn
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        skipConfirm || confirm("Close this tab?", busy: isBusy)
    }

    func windowWillClose(_ notification: Notification) {
        panes.forEach { $0.shutdown() }
        observer = nil
        Self.all.removeAll { $0 === self }
        if fillsScreen, !Self.all.contains(where: \.fillsScreen) {
            NSApp.presentationOptions = []
        }
        if Store.shared.bool("window.quit_when_last_closed"), Self.all.isEmpty {
            NSApp.terminate(nil)
        }
    }

    func windowDidBecomeKey(_ notification: Notification) {
        panes.forEach { $0.focused = $0 === active }
        updatePresentation()
    }

    func windowDidResize(_ notification: Notification) {
        host.frame = hostFrame
    }

    // MARK: Full screen

    /// xsh's full screen: the window covers its screen (below the camera
    /// housing) with a transparent title bar that keeps the window buttons,
    /// and the menu bar and Dock hide until the pointer reaches them.
    ///
    /// It does not use the native full screen. macOS shows the frames of a
    /// terminal in a native full-screen Space hundreds of milliseconds late,
    /// with the GPU and with the CPU renderer, while a window of the same
    /// size outside such a Space shows them at once.
    func toggleScreenFill() {
        guard let window else { return }
        let group = (window.tabbedWindows ?? [window]).compactMap { $0.windowController as? TerminalWindow }
        if let restore = screenFillRestore {
            for w in group {
                w.screenFillRestore = nil
                w.applyWindowSettings()
            }
            updatePresentation()
            window.setFrame(restore, display: true)
        } else {
            guard let screen = window.screen ?? NSScreen.main else { return }
            let restore = window.frame
            for w in group {
                w.screenFillRestore = restore
                w.applyWindowSettings()
            }
            // Hide the Dock and the menu bar first, so the frame can cover them.
            updatePresentation()
            var frame = screen.frame
            frame.size.height -= screen.safeAreaInsets.top
            window.setFrame(frame, display: true)
        }
        host.frame = hostFrame
    }

    /// A new tab in a window that fills the screen fills it too.
    func adoptScreenFill(from other: TerminalWindow) {
        guard let restore = other.screenFillRestore else { return }
        screenFillRestore = restore
        applyWindowSettings()
    }

    private func updatePresentation() {
        NSApp.presentationOptions = fillsScreen ? [.autoHideMenuBar, .autoHideDock] : []
    }

    // MARK: Tabs

    /// The `+` button of the tab bar.
    override func newWindowForTab(_ sender: Any?) {
        Terminals.newTab(profile: nil)
    }

    func addTab(_ other: TerminalWindow) {
        guard let w = window, let o = other.window else { return }
        w.addTabbedWindow(o, ordered: .above)
        o.makeKeyAndOrderFront(nil)
    }

    // MARK: Edit menu

    @objc func clearScrollback(_ sender: Any?) { active?.surface.clearScrollback() }
    @objc func resetTerminal(_ sender: Any?) { active?.surface.resetToInitialState() }

    @objc func biggerText(_ sender: Any?) { Terminals.changeFont(by: 1) }
    @objc func smallerText(_ sender: Any?) { Terminals.changeFont(by: -1) }
    @objc func defaultText(_ sender: Any?) { Terminals.changeFont(by: nil) }

    /// Runs a line in the active pane, for files and links from other apps.
    func type(_ line: String) {
        active?.surface.send(txt: line + "\n")
    }
}

/// Opens and restores windows.
enum Terminals {
    private static let sessionKey = "xsh.session"

    static var key: TerminalWindow? {
        NSApp.keyWindow?.windowController as? TerminalWindow
            ?? NSApp.mainWindow?.windowController as? TerminalWindow
    }

    /// The folder of a new tab or window.
    private static func directory() -> String? {
        let s = Store.shared
        switch s.string("window.new_tab_directory") {
        case "home": return nil
        case "custom":
            let d = s.string("window.custom_directory")
            return d.isEmpty ? nil : (d as NSString).expandingTildeInPath
        default: return key?.active?.cwd
        }
    }

    @discardableResult
    static func newWindow(profile: Profile?, directory dir: String? = nil) -> TerminalWindow {
        let w = TerminalWindow(layout: .pane(cwd: dir ?? directory(), profile: profile?.name))
        w.window?.center()
        if let k = key?.window, let o = w.window {
            o.setFrameOrigin(NSPoint(x: k.frame.minX + 24, y: k.frame.minY - 24))
        }
        w.showWindow(nil)
        w.syncTabBar()
        return w
    }

    @discardableResult
    static func newTab(profile: Profile?, directory dir: String? = nil) -> TerminalWindow {
        guard let cur = key else { return newWindow(profile: profile, directory: dir) }
        let w = TerminalWindow(
            layout: .pane(cwd: dir ?? directory(), profile: profile?.name),
            sizedFrom: cur.window)
        w.adoptScreenFill(from: cur)
        cur.addTab(w)
        return w
    }

    static func changeFont(by n: Int?) {
        Fonts.delta = n.map { Fonts.delta + $0 } ?? 0
        NotificationCenter.default.post(name: .xshSettingsChanged, object: nil)
    }

    // MARK: Session

    static func save() {
        var seen = Set<ObjectIdentifier>()
        var groups: [[[String: Any]]] = []
        var frames: [String] = []
        for w in NSApp.orderedWindows {
            guard w.windowController is TerminalWindow else { continue }
            let list = w.tabbedWindows ?? [w]
            guard let head = list.first, seen.insert(ObjectIdentifier(head)).inserted else { continue }
            groups.append(list.compactMap { ($0.windowController as? TerminalWindow)?.layoutSnapshot().plist })
            frames.append(NSStringFromRect(w.frame))
        }
        UserDefaults.standard.set(groups, forKey: sessionKey)
        UserDefaults.standard.set(frames, forKey: sessionKey + ".frames")
    }

    /// Opens the saved tabs. Returns false when nothing was saved.
    static func restore() -> Bool {
        guard Store.shared.bool("window.restore_session"),
              let groups = UserDefaults.standard.array(forKey: sessionKey) as? [[[String: Any]]],
              !groups.isEmpty
        else { return false }
        let frames = UserDefaults.standard.stringArray(forKey: sessionKey + ".frames") ?? []
        var opened = false
        for (idx, tabs) in groups.enumerated().reversed() {
            var firstWindow: TerminalWindow?
            let savedFrame: NSRect? = idx < frames.count ? {
                let r = NSRectFromString(frames[idx])
                return r.width >= 200 && r.height >= 120 ? r : nil
            }() : nil
            for layout in tabs.compactMap(Layout.init(plist:)) {
                if let f = firstWindow {
                    let w = TerminalWindow(layout: layout, sizedFrom: f.window)
                    f.addTab(w)
                } else {
                    let w = TerminalWindow(layout: layout)
                    if let savedFrame {
                        w.window?.setFrame(savedFrame, display: false)
                    } else {
                        w.window?.center()
                    }
                    w.showWindow(nil)
                    firstWindow = w
                    opened = true
                }
            }
            firstWindow?.syncTabBar()
        }
        return opened
    }
}
