import AppKit
import SwiftTerm

/// Stands between the terminal and the surface, because the surface answers
/// OSC 52 clipboard reads with no check and SwiftTerm has no switch for that.
final class TerminalGuard: TerminalViewDelegate {
    private weak var surface: Surface?

    init(_ surface: Surface) { self.surface = surface }

    func clipboardRead(source: TerminalView) -> Data? {
        Store.shared.bool("terminal.clipboard_read") ? surface?.clipboardRead(source: source) : nil
    }

    func clipboardCopy(source: TerminalView, content: Data) {
        surface?.clipboardCopy(source: source, content: content)
    }

    func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {
        surface?.sizeChanged(source: source, newCols: newCols, newRows: newRows)
    }

    func setTerminalTitle(source: TerminalView, title: String) {
        surface?.setTerminalTitle(source: source, title: title)
    }

    func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {
        surface?.hostCurrentDirectoryUpdate(source: source, directory: directory)
    }

    func send(source: TerminalView, data: ArraySlice<UInt8>) {
        surface?.send(source: source, data: data)
    }

    func scrolled(source: TerminalView, position: Double) {
        surface?.scrolled(source: source, position: position)
    }

    func rangeChanged(source: TerminalView, startY: Int, endY: Int) {
        surface?.rangeChanged(source: source, startY: startY, endY: endY)
    }

    func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {
        surface?.requestOpenLink(source: source, link: link, params: params)
    }

    func programStatusChanged(source: TerminalView, records: [TerminalProgramStatus]) {
        surface?.programStatusChanged(source: source, records: records)
    }
}

/// The terminal itself, with paste checks and file drops.
final class Surface: LocalProcessTerminalView {
    /// `terminalDelegate` is weak, so the surface keeps the guard alive.
    private var guardian: TerminalGuard?

    func installGuard() {
        let g = TerminalGuard(self)
        guardian = g
        terminalDelegate = g
    }

    /// The GPU view inside the terminal cannot take the focus. A click that
    /// lands on it must count as a click on the terminal, or the keys stay in
    /// another pane. The scroller keeps its own clicks.
    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        return hit == nil || hit is NSScroller ? hit : self
    }

    override func paste(_ sender: Any) {
        let text = NSPasteboard.general.string(forType: .string) ?? ""
        let multi = text.trimmingCharacters(in: .newlines).contains(where: \.isNewline)
        // With bracketed paste the program sees it as text and runs nothing.
        if multi, Store.shared.bool("terminal.paste_warning"), !terminalStateSnapshot().bracketedPasteMode {
            let a = NSAlert()
            a.messageText = "Paste more than one line?"
            a.informativeText = "The shell can run each line when it is pasted.\n\n" + String(text.prefix(400))
            a.addButton(withTitle: "Paste")
            a.addButton(withTitle: "Cancel")
            guard a.runModal() == .alertFirstButtonReturn else { return }
        }
        super.paste(sender)
    }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        sender.draggingPasteboard.canReadObject(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true])
            ? .copy : []
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let opts: [NSPasteboard.ReadingOptionKey: Any] = [.urlReadingFileURLsOnly: true]
        guard let urls = sender.draggingPasteboard.readObjects(forClasses: [NSURL.self], options: opts) as? [URL],
              !urls.isEmpty
        else { return false }
        pasteText(urls.map { shellQuote($0.path) }.joined(separator: " ") + " ")
        return true
    }
}

/// Darkens a pane, and lets clicks go through to the terminal.
private final class DimLayer: NSView {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

/// The bar above a pane when a tab has more than one pane.
private final class PaneHeader: NSView {
    static let height: CGFloat = 22

    let label = NSTextField(labelWithString: "")
    let closeButton: NSButton
    var onClick: (() -> Void)?
    var onClose: (() -> Void)?

    override init(frame: NSRect) {
        let image = NSImage(systemSymbolName: "xmark", accessibilityDescription: "Close Pane")
            ?? NSImage(named: NSImage.stopProgressTemplateName)
            ?? NSImage()
        closeButton = NSButton(image: image, target: nil, action: nil)
        super.init(frame: frame)
        label.lineBreakMode = .byTruncatingMiddle
        label.font = .systemFont(ofSize: 11)
        addSubview(label)
        closeButton.isBordered = false
        closeButton.imageScaling = .scaleProportionallyDown
        closeButton.toolTip = "Close Pane"
        closeButton.target = self
        closeButton.action = #selector(closeClicked)
        addSubview(closeButton)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    override var mouseDownCanMoveWindow: Bool { false }

    override func layout() {
        super.layout()
        let side = bounds.height
        closeButton.frame = NSRect(x: bounds.width - side - 2, y: 0, width: side, height: side).insetBy(dx: 5, dy: 5)
        let textHeight = label.intrinsicContentSize.height
        label.frame = NSRect(
            x: 8,
            y: (bounds.height - textHeight) / 2,
            width: max(bounds.width - side - 14, 0),
            height: textHeight)
    }

    override func mouseDown(with event: NSEvent) { onClick?() }

    @objc private func closeClicked() { onClose?() }
}

/// One pane: a terminal with padding, a bar with a close button when the tab
/// has more panes, and an optional dim layer when it has no focus.
final class Pane: NSView, LocalProcessTerminalViewDelegate {
    let surface: Surface
    let profile: Profile?
    private let dim = DimLayer()
    private let header = PaneHeader()
    private let agentSocket: String?
    private var oscObservation: TerminalOscObservation?
    private var font: NSFont?
    private var usingGPU: Bool?
    private var scrollback: Int?
    private(set) var title = ""
    private(set) var exited = false
    weak var owner: TerminalWindow?

    var focused = true {
        didSet {
            updateDim()
            updateHeader()
        }
    }

    var showsHeader = false {
        didSet {
            header.isHidden = !showsHeader
            updateHeader()
            needsLayout = true
            needsDisplay = true
        }
    }

    /// With `agentSocket`, the pane runs the commands of the agent at that socket.
    init(profile: Profile?, directory: String?, frame: NSRect, agentSocket: String? = nil) {
        self.profile = profile
        self.agentSocket = agentSocket
        let options = TerminalOptions(
            termName: Store.shared.string("terminal.term"),
            screenReaderMode: Store.shared.bool("ui.screen_reader"),
            scrollback: Store.shared.int("terminal.scrollback"))
        surface = Surface(frame: frame, font: Fonts.make(profile: profile), options: options)
        super.init(frame: frame)
        wantsLayer = true
        autoresizingMask = [.width, .height]
        surface.installGuard()
        surface.processDelegate = self
        surface.registerForDraggedTypes([.fileURL])
        addSubview(surface)
        header.isHidden = true
        header.onClick = { [weak self] in
            guard let self else { return }
            owner?.focus(self)
        }
        header.onClose = { [weak self] in
            guard let self else { return }
            owner?.requestClose(self)
        }
        addSubview(header)
        dim.wantsLayer = true
        dim.isHidden = true
        addSubview(dim)
        apply()
        observeAgentRequests()
        start(directory: directory)
    }

    private func observeAgentRequests() {
        oscObservation = surface.observeOscEvents { [weak self] event in
            guard event.code == 7741 else { return }
            let text = String(decoding: event.payload, as: UTF8.self)
            DispatchQueue.main.async { self?.handleAgentRequest(text) }
        }
    }

    /// Format: `split;<token>;<socket path>`.
    private func handleAgentRequest(_ text: String) {
        let parts = text.split(separator: ";", maxSplits: 2).map(String.init)
        guard parts.count == 3, parts[0] == "split", parts[1] == Session.agentToken, parts[2].hasPrefix("/"),
              FileManager.default.fileExists(atPath: parts[2])
        else { return }
        owner?.openAgentPane(beside: self, socket: parts[2])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    var isBusy: Bool { !exited && Session.hasChildren(surface.process.shellPid) }
    var cwd: String? { Session.cwd(surface.process.shellPid) }

    private func start(directory: String?) {
        let s = Store.shared
        let custom = agentSocket != nil ? nil : [profile?.shell ?? "", s.string("terminal.shell")].first { !$0.isEmpty }
        let program = custom ?? XshCLI.binary.path
        let args = agentSocket.map { ["app", "agent-runner", $0] }
            ?? ((profile?.shell.isEmpty ?? true) ? s.strings("terminal.args") + (profile?.args ?? []) : profile?.args ?? [])
        let base = URL(fileURLWithPath: program).lastPathComponent
        let login = custom != nil && s.bool("terminal.login_shell")
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let dir = [directory ?? "", profile?.directory ?? ""].first { !$0.isEmpty } ?? home
        surface.startProcess(
            executable: program,
            args: args,
            environment: Session.environment(extra: profile?.env ?? []),
            execName: login ? "-" + base : nil,
            currentDirectory: (dir as NSString).expandingTildeInPath)
    }

    /// Applies the current settings. Safe to call at any time.
    func apply() {
        let s = Store.shared
        let p = Palette.current(profile: profile)
        let opacity = CGFloat(min(max(s.int("window.opacity"), 10), 100)) / 100
        let t = surface

        let newFont = Fonts.make(profile: profile)
        if newFont != font {
            // Setting the font clears the selection, so only do it for a change.
            if font != nil { t.font = newFont }
            font = newFont
        }
        t.lineSpacing = CGFloat(max(s.int("font.line_spacing"), 50)) / 100
        t.fontSmoothing = s.bool("font.smoothing")
        t.useBrightColors = s.bool("font.bright_bold")
        t.customBlockGlyphs = s.bool("font.custom_block_glyphs")
        t.minimumContrastRatio = CGFloat(max(s.int("font.min_contrast"), 1))

        t.nativeForegroundColor = p.foreground
        t.nativeBackgroundColor = p.background.withAlphaComponent(opacity)
        t.caretColor = p.cursor
        t.selectedTextBackgroundColor = p.selection
        if let a = p.ansi { t.installColors(a) }

        let blink = s.bool("terminal.cursor_blink")
        switch s.string("terminal.cursor_style") {
        case "underline": t.setCursorStyle(blink ? .blinkUnderline : .steadyUnderline)
        case "bar": t.setCursorStyle(blink ? .blinkBar : .steadyBar)
        default: t.setCursorStyle(blink ? .blinkBlock : .steadyBlock)
        }
        t.optionAsMetaKey = s.bool("terminal.option_as_meta")
        t.allowMouseReporting = s.bool("terminal.mouse_reporting")
        t.backspaceSendsControlH = s.bool("terminal.backspace_ctrl_h")
        t.scrollSensitivity = CGFloat(max(s.int("terminal.scroll_speed"), 10)) / 100
        t.scrollerStyle = s.string("terminal.scroller") == "legacy" ? .legacy : .overlay
        t.linkHighlightMode = s.string("terminal.links") == "click" ? .hover : .hoverWithModifier
        switch s.string("terminal.bell") {
        case "none": t.bellStyle = .none
        case "sound": t.bellStyle = .sound
        case "both": t.bellStyle = .soundAndVisual
        default: t.bellStyle = .visual
        }

        let lines = s.int("terminal.scrollback")
        if lines != scrollback {
            if scrollback != nil { t.changeScrollback(lines > 0 ? lines : nil) }
            scrollback = lines
        }
        let gpu = s.bool("terminal.gpu")
        if gpu != usingGPU {
            try? t.setUseMetal(gpu)
            usingGPU = gpu
        }
        layer?.backgroundColor = NSColor.clear.cgColor
        updateDim()
        updateHeader()
        needsLayout = true
        needsDisplay = true
    }

    private func updateDim() {
        let on = !focused && Store.shared.bool("window.dim_inactive_panes")
        dim.isHidden = !on
        dim.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.25).cgColor
    }

    private func updateHeader() {
        guard showsHeader else { return }
        let folder = cwd.map { ($0 as NSString).abbreviatingWithTildeInPath }
        header.label.stringValue = title.isEmpty ? folder ?? "xsh" : title
        let color = Palette.current(profile: profile).foreground.withAlphaComponent(focused ? 1 : 0.55)
        header.label.textColor = color
        header.closeButton.contentTintColor = color
    }

    override func layout() {
        super.layout()
        let pad = CGFloat(Store.shared.int("window.padding"))
        let top = showsHeader ? max(PaneHeader.height, pad) : pad
        surface.frame = NSRect(
            x: pad,
            y: pad,
            width: max(bounds.width - pad * 2, 0),
            height: max(bounds.height - pad - top, 0))
        header.frame = NSRect(x: 0, y: bounds.height - PaneHeader.height, width: bounds.width, height: PaneHeader.height)
        dim.frame = bounds
    }

    /// Paints only the padding: the terminal paints its own background, and
    /// two layers of a translucent color would look darker.
    override func draw(_ dirtyRect: NSRect) {
        let opacity = CGFloat(min(max(Store.shared.int("window.opacity"), 10), 100)) / 100
        Palette.current(profile: profile).background.withAlphaComponent(opacity).setFill()
        let ring = NSBezierPath(rect: bounds)
        ring.append(NSBezierPath(rect: surface.frame))
        ring.windingRule = .evenOdd
        ring.fill()
    }

    func shutdown() {
        oscObservation = nil
        if !exited { surface.terminate() }
        surface.processDelegate = nil
        surface.updateUiClosed()
        removeFromSuperview()
    }

    // MARK: LocalProcessTerminalViewDelegate

    func sizeChanged(source: LocalProcessTerminalView, newCols: Int, newRows: Int) {}

    func setTerminalTitle(source: LocalProcessTerminalView, title: String) {
        self.title = title
        updateHeader()
        owner?.updateTitle()
    }

    func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {
        updateHeader()
        owner?.updateTitle()
    }

    func processTerminated(source: TerminalView, exitCode: Int32?) {
        exited = true
        let mode = Store.shared.string("window.on_exit")
        let close = mode == "close" || (mode == "close_clean" && exitCode == 0)
        if close {
            owner?.remove(self)
        } else {
            let text = exitCode.map { "status \($0)" } ?? "a signal"
            surface.feed(text: "\r\n[The process stopped with \(text). Press \u{2318}W to close.]\r\n")
        }
    }

    func processFailedToStart(source: TerminalView, error: LocalProcessError) {
        exited = true
    }
}
