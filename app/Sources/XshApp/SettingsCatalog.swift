/// Every setting in the Settings window. Rows are drawn from this list, so a
/// new setting is one line here plus its field in `rushy-config`.
struct Setting: Identifiable {
    enum Kind {
        case toggle
        case number(ClosedRange<Int>, step: Int, unit: String)
        case slider(ClosedRange<Int>, unit: String)
        case decimal(ClosedRange<Double>, step: Double)
        case choice([(value: String, label: String)])
        case text(String)
        case list(String)
        case folder
        case file
        case color
        case font
        case theme
        case shortcut
    }

    let id: String
    let label: String
    let help: String
    let kind: Kind

    init(_ id: String, _ label: String, _ kind: Kind, _ help: String = "") {
        self.id = id
        self.label = label
        self.kind = kind
        self.help = help
    }
}

struct SettingGroup: Identifiable {
    let title: String
    let items: [Setting]
    /// Found by search, but drawn on its page by the page's own view.
    let searchOnly: Bool
    var id: String { title }

    init(_ title: String, _ items: [Setting], searchOnly: Bool = false) {
        self.title = title
        self.items = items
        self.searchOnly = searchOnly
    }
}

enum PageExtra { case general, themes, profiles, bundles, history, ai, advanced }

struct SettingsPage: Identifiable, Hashable {
    let id: String
    let title: String
    let icon: String
    let groups: [SettingGroup]
    var extra: PageExtra?

    static func == (a: SettingsPage, b: SettingsPage) -> Bool { a.id == b.id }
    func hash(into h: inout Hasher) { h.combine(id) }
}

enum Catalog {
    static let pages: [SettingsPage] = [
        SettingsPage(id: "general", title: "General", icon: "gearshape", groups: [
            SettingGroup("Updates", [
                Setting("updates.auto_check", "Look for new versions once a day", .toggle, "xsh asks before it installs a new version."),
            ]),
            SettingGroup("Start", [
                Setting("window.restore_session", "Open the last tabs at launch", .toggle,
                        "Opens the same tabs, splits, and folders again. Programs do not start again."),
                Setting("window.quit_when_last_closed", "Quit when the last window closes", .toggle),
                Setting("window.new_tab_directory", "New tabs open in", .choice([
                    ("previous", "The folder of the current tab"), ("home", "The home folder"), ("custom", "A set folder"),
                ])),
                Setting("window.custom_directory", "Set folder", .folder),
            ]),
            SettingGroup("Close", [
                Setting("window.confirm_close", "Ask before closing", .choice([
                    ("processes", "When a program runs"), ("always", "Always"), ("never", "Never"),
                ])),
                Setting("window.on_exit", "When the shell stops", .choice([
                    ("close", "Close the pane"), ("close_clean", "Close the pane if there was no error"), ("keep", "Keep the pane open"),
                ])),
            ]),
        ], extra: .general),

        SettingsPage(id: "appearance", title: "Appearance", icon: "paintpalette", groups: [
            SettingGroup("Theme", [
                Setting("window.appearance", "App appearance", .choice([
                    ("system", "Same as the system"), ("light", "Light"), ("dark", "Dark"),
                ])),
                Setting("ui.theme", "Theme", .theme),
                Setting("window.light_theme", "Theme in light mode", .theme, "Empty uses the theme above."),
                Setting("window.dark_theme", "Theme in dark mode", .theme, "Empty uses the theme above."),
            ]),
            SettingGroup("Color overrides", [
                Setting("colors.background", "Background", .color),
                Setting("colors.foreground", "Text", .color),
                Setting("colors.cursor", "Cursor", .color),
                Setting("colors.selection", "Selection", .color),
            ]),
        ], extra: .themes),

        SettingsPage(id: "font", title: "Text", icon: "textformat", groups: [
            SettingGroup("Font", [
                Setting("font.family", "Font", .font),
                Setting("font.size", "Size", .number(6...96, step: 1, unit: "pt")),
                Setting("font.line_spacing", "Line height", .slider(80...200, unit: "%")),
                Setting("font.smoothing", "Smooth the text", .toggle),
            ]),
            SettingGroup("Drawing", [
                Setting("font.bright_bold", "Show bold text in bright colors", .toggle),
                Setting("font.custom_block_glyphs", "Draw box and block characters with built-in shapes", .toggle,
                        "This keeps lines joined with no gaps."),
                Setting("font.min_contrast", "Lowest text contrast", .slider(1...21, unit: ":1"),
                        "1 turns this off. Higher values change colors to make text easier to read."),
            ]),
        ]),

        SettingsPage(id: "window", title: "Window", icon: "macwindow", groups: [
            SettingGroup("Size", [
                Setting("window.columns", "Columns", .number(20...500, step: 1, unit: "")),
                Setting("window.rows", "Rows", .number(5...200, step: 1, unit: "")),
                Setting("window.padding", "Padding", .number(0...64, step: 1, unit: "pt")),
            ]),
            SettingGroup("Style", [
                Setting("window.titlebar", "Title bar", .choice([
                    ("native", "Standard"), ("transparent", "Transparent"), ("hidden", "Hidden"),
                ])),
                Setting("window.tab_bar", "Tab bar", .choice([
                    ("auto", "With two or more tabs"), ("always", "Always"),
                ])),
                Setting("window.opacity", "Background opacity", .slider(10...100, unit: "%")),
                Setting("window.blur", "Blur behind the window", .toggle, "Only applies when the opacity is below 100%."),
                Setting("window.always_on_top", "Keep above other windows", .toggle),
                Setting("window.dim_inactive_panes", "Dim panes without focus", .toggle),
            ]),
        ]),

        SettingsPage(id: "terminal", title: "Terminal", icon: "terminal", groups: [
            SettingGroup("Session", [
                Setting("terminal.shell", "Program", .file, "Empty runs xsh. Set a path to use another shell, such as /bin/zsh."),
                Setting("terminal.args", "Arguments", .list("One argument per line")),
                Setting("terminal.login_shell", "Start another shell as a login shell", .toggle),
                Setting("terminal.env", "Environment variables", .list("NAME=value, one per line")),
                Setting("terminal.term", "TERM", .text("xterm-256color")),
                Setting("terminal.scrollback", "Scrollback lines", .number(0...1_000_000, step: 1000, unit: ""), "0 turns the scrollback off."),
            ]),
            SettingGroup("Cursor", [
                Setting("terminal.cursor_style", "Shape", .choice([
                    ("block", "Block"), ("underline", "Underline"), ("bar", "Bar"),
                ])),
                Setting("terminal.cursor_blink", "Blink", .toggle),
            ]),
            SettingGroup("Keyboard and mouse", [
                Setting("terminal.option_as_meta", "Option key sends Meta", .toggle,
                        "Turn this off to type special characters with Option."),
                Setting("terminal.backspace_ctrl_h", "Delete key sends Ctrl-H", .toggle),
                Setting("terminal.mouse_reporting", "Programs can read the mouse", .toggle),
                Setting("terminal.copy_on_select", "Copy text when it is selected", .toggle),
                Setting("terminal.paste_warning", "Ask before a paste with more than one line", .toggle),
                Setting("terminal.clipboard_read", "Programs can read the clipboard", .toggle,
                        "Off is safer: a program, also on a remote computer over ssh, could read your passwords and tokens."),
                Setting("terminal.links", "Open links with", .choice([
                    ("command_click", "⌘-click"), ("click", "Click"),
                ])),
                Setting("terminal.scroll_speed", "Scroll speed", .slider(10...400, unit: "%")),
                Setting("terminal.scroller", "Scroll bar", .choice([
                    ("overlay", "Show when scrolling"), ("legacy", "Always show"),
                ])),
            ]),
            SettingGroup("Alerts and drawing", [
                Setting("terminal.bell", "Bell", .choice([
                    ("visual", "Flash"), ("sound", "Sound"), ("both", "Flash and sound"), ("none", "Off"),
                ])),
                Setting("terminal.gpu", "Draw with the GPU (Metal)", .toggle),
            ]),
        ]),

        SettingsPage(id: "keys", title: "Shortcuts", icon: "command", groups: [
            SettingGroup("Global", [
                Setting("keys.global_hotkey", "Show or hide xsh from any app", .shortcut),
            ]),
            SettingGroup("Windows and tabs", [
                Setting("keys.new_window", "New window", .shortcut),
                Setting("keys.new_tab", "New tab", .shortcut),
                Setting("keys.close", "Close pane or tab", .shortcut),
                Setting("keys.next_tab", "Next tab", .shortcut),
                Setting("keys.previous_tab", "Previous tab", .shortcut),
                Setting("keys.fullscreen", "Full screen", .shortcut),
                Setting("keys.settings", "Settings", .shortcut),
            ]),
            SettingGroup("Panes", [
                Setting("keys.split_right", "Split right", .shortcut),
                Setting("keys.split_down", "Split down", .shortcut),
                Setting("keys.equalize_panes", "Make panes equal", .shortcut),
                Setting("keys.next_pane", "Next pane", .shortcut),
                Setting("keys.previous_pane", "Previous pane", .shortcut),
                Setting("keys.focus_left", "Pane to the left", .shortcut),
                Setting("keys.focus_right", "Pane to the right", .shortcut),
                Setting("keys.focus_up", "Pane above", .shortcut),
                Setting("keys.focus_down", "Pane below", .shortcut),
            ]),
            SettingGroup("Text", [
                Setting("keys.find", "Find", .shortcut),
                Setting("keys.find_next", "Find next", .shortcut),
                Setting("keys.find_previous", "Find previous", .shortcut),
                Setting("keys.clear_scrollback", "Clear scrollback", .shortcut),
                Setting("keys.reset", "Reset terminal", .shortcut),
                Setting("keys.font_bigger", "Bigger text", .shortcut),
                Setting("keys.font_smaller", "Smaller text", .shortcut),
                Setting("keys.font_reset", "Default text size", .shortcut),
            ]),
        ]),

        SettingsPage(id: "profiles", title: "Profiles", icon: "person.2", groups: [], extra: .profiles),

        SettingsPage(id: "shell", title: "Shell", icon: "chevron.left.forwardslash.chevron.right", groups: [
            SettingGroup("Line editor", [
                Setting("shell.edit_mode", "Key mode", .choice([("emacs", "Emacs"), ("vi", "Vi")])),
                Setting("shell.autosuggest", "Suggest from history while typing", .toggle),
                Setting("ui.highlight", "Color the command line", .toggle),
                Setting("ui.right_prompt", "Show status and duration on the right", .toggle),
                Setting("shell.suggest_typos", "Suggest close names for an unknown command", .toggle),
            ]),
            SettingGroup("Behaviour", [
                Setting("shell.auto_cd", "A folder name alone changes to it", .toggle),
                Setting("shell.cdpath", "Folders that cd also searches", .list("One folder per line")),
                Setting("shell.confirm_exit_with_jobs", "Ask before exit when background jobs run", .toggle),
                Setting("shell.notify_after_secs", "Notice for commands longer than", .number(0...86_400, step: 5, unit: "s"), "0 turns this off."),
                Setting("shell.capture_stderr", "Keep error output for ai fix", .toggle),
                Setting("shell.terminal_integration", "Send title, folder, and prompt marks to the terminal", .toggle),
            ]),
            SettingGroup("Start-up", [
                Setting("shell.rc_file", "Start-up file", .text("init.xsh"), "Relative to the config folder."),
                Setting("shell.import_rc", "Import variables and aliases from", .choice([
                    ("auto", "My login shell"), ("zsh", "zsh"), ("bash", "bash"), ("off", "Nothing"),
                ])),
            ]),
            SettingGroup("Accessibility", [
                Setting("ui.screen_reader", "Screen reader mode", .toggle, "Plain symbols, no animation. New panes use it."),
                Setting("ui.no_color", "Turn colors off in the shell", .toggle),
                Setting("ui.hot_reload", "Reload when the config folder changes", .toggle),
            ]),
        ]),

        SettingsPage(id: "mux", title: "Multiplexer", icon: "rectangle.split.3x1", groups: [
            SettingGroup("xsh new and xsh attach", [
                Setting("mux.prefix", "Prefix key", .text("C-b")),
                Setting("mux.shell", "Program in each pane", .file, "Empty runs xsh."),
                Setting("ui.status_bar", "Show the status bar", .toggle),
            ]),
        ]),

        SettingsPage(id: "history", title: "History", icon: "clock.arrow.circlepath", groups: [
            SettingGroup("Recording", [
                Setting("history.enabled", "Record commands (encrypted)", .toggle),
                Setting("history.ignore_dups", "Skip a command that repeats the last one", .toggle),
                Setting("history.ignore_space", "Skip a command that starts with a space", .toggle),
                Setting("history.editor_entries", "Entries for Ctrl-R", .number(0...1_000_000, step: 500, unit: "")),
                Setting("history.key_source", "Encryption key from", .choice([
                    ("auto", "Keychain, then passphrase"), ("keyring", "Keychain"), ("passphrase", "Passphrase variable"),
                ])),
            ]),
        ], extra: .history),

        SettingsPage(id: "bundles", title: "Bundles", icon: "shippingbox", groups: [
            SettingGroup("Security", [
                Setting("bundles.allow_hooks", "Run bundle hooks (after the safety guard)", .toggle),
                Setting("bundles.registry_url", "Registry", .text("https://…")),
                Setting("bundles.allowed_hosts", "Allowed download hosts", .list("One host per line")),
            ]),
        ], extra: .bundles),

        SettingsPage(id: "ai", title: "AI", icon: "sparkles", groups: [
            SettingGroup("Provider", [
                Setting("ai.default.provider", "Provider", .choice([
                    ("openai", "OpenAI"), ("anthropic", "Anthropic"), ("ollama", "Ollama (local)"),
                    ("custom", "Custom"),
                ])),
                Setting("ai.default.model", "Model", .text("Provider default")),
                Setting("ai.providers.custom.base_url", "Custom endpoint", .text("http://127.0.0.1:9694/v1")),
                Setting("ai.providers.custom.model", "Custom model", .text("Fetch models or type a name")),
            ], searchOnly: true),
            SettingGroup("Model settings", [
                Setting("ai.default.temperature", "Temperature", .decimal(0...2, step: 0.1)),
                Setting("ai.default.max_tokens", "Answer limit", .number(16...200_000, step: 256, unit: "tokens")),
                Setting("ai.default.max_context_tokens", "Context limit", .number(0...2_000_000, step: 1000, unit: "tokens")),
            ]),
            SettingGroup("Safety", [
                Setting("ai.default.safety_review", "Ask the model to review each command", .toggle),
                Setting("ai.default.approve_each_step", "Ask before each agent step", .toggle),
                Setting("ai.default.autopilot", "Agent autopilot", .toggle,
                        "Runs each command without a question, up to 50 steps. The safety checks stay on. Ctrl-C stops the agent."),
            ]),
            SettingGroup("What each request includes", [
                Setting("ai.context.listing", "Files in the current folder", .toggle),
                Setting("ai.context.shell", "The value of $SHELL", .toggle),
                Setting("ai.context.bundles", "Active bundle names", .toggle),
                Setting("ai.context.git_status", "git status, for ai fix", .toggle),
                Setting("ai.context.history", "Recent commands", .number(0...500, step: 5, unit: "")),
            ]),
        ], extra: .ai),

        SettingsPage(id: "advanced", title: "Advanced", icon: "wrench.and.screwdriver", groups: [], extra: .advanced),
    ]

    static var all: [Setting] { pages.flatMap { $0.groups.flatMap(\.items) } }
}
