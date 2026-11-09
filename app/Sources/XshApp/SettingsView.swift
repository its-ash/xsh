import AppKit
import SwiftUI
import UniformTypeIdentifiers

final class SettingsHost: NSHostingController<SettingsRoot> {
    init() { super.init(rootView: SettingsRoot()) }

    @available(*, unavailable)
    @MainActor required dynamic init?(coder: NSCoder) { nil }
}

struct SettingsRoot: View {
    @ObservedObject private var store = Store.shared
    @State private var page: SettingsPage? = Catalog.pages.first
    @State private var search = ""

    private var hits: [Setting] {
        let q = search.lowercased()
        return Catalog.all.filter { "\($0.label) \($0.help) \($0.id)".lowercased().contains(q) }
    }

    var body: some View {
        NavigationSplitView {
            List(Catalog.pages, selection: $page) { p in
                NavigationLink(value: p) { Label(p.title, systemImage: p.icon) }
            }
            .navigationSplitViewColumnWidth(190)
            .searchable(text: $search, placement: .sidebar, prompt: "Search settings")
        } detail: {
            VStack(spacing: 0) {
                if let e = store.saveError {
                    Text(e)
                        .font(.callout)
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(10)
                        .background(.red)
                }
                if !search.isEmpty {
                    Form {
                        ForEach(hits) { SettingRow(s: $0) }
                        if hits.isEmpty { Text("No setting matches.").foregroundStyle(.secondary) }
                    }
                    .formStyle(.grouped)
                } else if let page {
                    PageView(page: page).id(page.id)
                }
            }
            .navigationTitle(search.isEmpty ? page?.title ?? "Settings" : "Search")
        }
    }
}

struct PageView: View {
    let page: SettingsPage

    var body: some View {
        Form {
            switch page.extra {
            case .general: GeneralExtra()
            case .themes: ThemesExtra()
            case .ai: AIKeysExtra()
            default: EmptyView()
            }
            ForEach(page.groups.filter { !$0.searchOnly }) { g in
                Section(g.title) {
                    ForEach(g.items) { SettingRow(s: $0) }
                }
            }
            switch page.extra {
            case .themes: AnsiExtra()
            case .profiles: ProfilesExtra()
            case .bundles: BundlesExtra()
            case .history: HistoryExtra()
            case .advanced: AdvancedExtra()
            default: EmptyView()
            }
        }
        .formStyle(.grouped)
    }
}

// MARK: - Rows

struct SettingRow: View {
    let s: Setting
    @ObservedObject private var store = Store.shared

    private var stringBinding: Binding<String> {
        Binding { store.string(s.id) } set: { store.set(s.id, $0) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            control
            if !s.help.isEmpty {
                Text(s.help).font(.caption).foregroundStyle(.secondary)
            }
        }
        .contextMenu {
            Button("Reset to Default") { store.unset(s.id) }
        }
    }

    @ViewBuilder private var control: some View {
        switch s.kind {
        case .toggle:
            Toggle(s.label, isOn: Binding { store.bool(s.id) } set: { store.set(s.id, $0) })
        case let .number(range, step, unit):
            NumberRow(label: s.label, range: range, step: step, unit: unit, key: s.id)
        case let .slider(range, unit):
            LabeledContent(s.label) {
                HStack {
                    Slider(
                        value: Binding { Double(store.int(s.id)) } set: { store.set(s.id, Int($0.rounded())) },
                        in: Double(range.lowerBound)...Double(range.upperBound))
                    .frame(maxWidth: 220)
                    Text("\(store.int(s.id))\(unit)").monospacedDigit().frame(width: 52, alignment: .trailing)
                }
            }
        case let .decimal(range, step):
            LabeledContent(s.label) {
                HStack {
                    Slider(
                        value: Binding { store.double(s.id) } set: { store.set(s.id, ($0 * 100).rounded() / 100) },
                        in: range, step: step)
                    .frame(maxWidth: 220)
                    Text(String(format: "%.1f", store.double(s.id))).monospacedDigit().frame(width: 40, alignment: .trailing)
                }
            }
        case let .choice(items):
            Picker(s.label, selection: stringBinding) {
                ForEach(items, id: \.value) { Text($0.label).tag($0.value) }
            }
        case let .text(hint):
            LabeledContent(s.label) {
                TextField("", text: stringBinding, prompt: Text(hint))
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 300)
            }
        case let .list(hint):
            LabeledContent(s.label) {
                ListField(key: s.id, hint: hint)
            }
        case .folder:
            PathRow(label: s.label, key: s.id, folders: true)
        case .file:
            PathRow(label: s.label, key: s.id, folders: false)
        case .color:
            ColorRow(label: s.label, key: s.id)
        case .font:
            Picker(s.label, selection: stringBinding) {
                Text("System monospaced").tag("")
                ForEach(Fonts.fixedPitchFamilies, id: \.self) { Text($0).tag($0) }
            }
        case .theme:
            Picker(s.label, selection: stringBinding) {
                if s.id != "ui.theme" { Text("None").tag("") }
                ForEach(store.themes.compactMap { $0["name"] as? String }, id: \.self) { Text($0).tag($0) }
            }
        case .shortcut:
            LabeledContent(s.label) { ShortcutField(key: s.id) }
        }
    }
}

struct NumberRow: View {
    let label: String
    let range: ClosedRange<Int>
    let step: Int
    let unit: String
    let key: String
    @ObservedObject private var store = Store.shared

    var body: some View {
        LabeledContent(label) {
            HStack(spacing: 6) {
                TextField("", value: Binding {
                    store.int(key)
                } set: {
                    store.set(key, min(max($0, range.lowerBound), range.upperBound))
                }, format: .number.grouping(.never))
                .textFieldStyle(.roundedBorder)
                .multilineTextAlignment(.trailing)
                .frame(width: 80)
                Stepper("", value: Binding {
                    store.int(key)
                } set: {
                    store.set(key, $0)
                }, in: range, step: step)
                .labelsHidden()
                if !unit.isEmpty { Text(unit).foregroundStyle(.secondary) }
            }
        }
    }
}

struct ListField: View {
    let key: String
    let hint: String
    @ObservedObject private var store = Store.shared

    var body: some View {
        TextField("", text: Binding {
            store.strings(key).joined(separator: "\n")
        } set: { text in
            store.set(key, text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init))
        }, prompt: Text(hint), axis: .vertical)
        .lineLimit(2...6)
        .textFieldStyle(.roundedBorder)
        .frame(maxWidth: 300)
    }
}

struct PathRow: View {
    let label: String
    let key: String
    let folders: Bool
    @ObservedObject private var store = Store.shared

    var body: some View {
        LabeledContent(label) {
            HStack {
                TextField("", text: Binding { store.string(key) } set: { store.set(key, $0) })
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 240)
                Button("Choose…") {
                    let p = NSOpenPanel()
                    p.canChooseFiles = !folders
                    p.canChooseDirectories = folders
                    p.allowsMultipleSelection = false
                    if p.runModal() == .OK, let u = p.url { store.set(key, u.path) }
                }
            }
        }
    }
}

struct ColorRow: View {
    let label: String
    let key: String
    @ObservedObject private var store = Store.shared

    var body: some View {
        LabeledContent(label) {
            HStack {
                if !store.string(key).isEmpty {
                    Button("Use Theme") { store.set(key, "") }.buttonStyle(.link)
                }
                ColorPicker("", selection: Binding {
                    Color(nsColor: NSColor(hex: store.string(key)) ?? fallback)
                } set: {
                    store.set(key, NSColor($0).hexString)
                }, supportsOpacity: false)
                .labelsHidden()
            }
        }
    }

    private var fallback: NSColor {
        let p = Palette.current(profile: nil)
        switch key {
        case "colors.foreground": return p.foreground
        case "colors.cursor": return p.cursor
        case "colors.selection": return p.selection
        default: return p.background
        }
    }
}

struct ShortcutField: View {
    let key: String
    @ObservedObject private var store = Store.shared
    @State private var recording = false
    @State private var monitor: Any?

    var body: some View {
        HStack {
            Button(recording ? "Press keys…" : Shortcut.display(store.string(key))) {
                recording ? stop() : start()
            }
            .frame(minWidth: 110)
            if !store.string(key).isEmpty {
                Button {
                    store.set(key, "")
                } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help("Turn this shortcut off")
            }
        }
        .onDisappear { stop() }
    }

    private func start() {
        recording = true
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { e in
            if Int(e.keyCode) == 53 {
                stop()
            } else if let text = Shortcut.text(from: e) {
                store.set(key, text)
                stop()
            }
            return nil
        }
    }

    private func stop() {
        recording = false
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
    }
}

// MARK: - General

struct GeneralExtra: View {
    @ObservedObject private var store = Store.shared
    @State private var note = ""

    var body: some View {
        Section("xsh on this Mac") {
            LabeledContent("Default terminal") {
                Button("Make xsh the Default") { makeDefault() }
            }
            Text("Scripts (.command, .tool) and ssh links then open in xsh.")
                .font(.caption).foregroundStyle(.secondary)
            LabeledContent("Command-line tool") {
                Button("Install xsh in ~/.local/bin") { installTool() }
            }
            if !note.isEmpty { Text(note).font(.caption).foregroundStyle(.secondary) }
        }
    }

    private func makeDefault() {
        let app = Bundle.main.bundleURL
        let ws = NSWorkspace.shared
        var types = [UTType.unixExecutable]
        if let t = UTType("com.apple.terminal.shell-script") { types.append(t) }
        let group = DispatchGroup()
        var failure: String?
        for t in types {
            group.enter()
            ws.setDefaultApplication(at: app, toOpen: t) { e in
                if let e { failure = e.localizedDescription }
                group.leave()
            }
        }
        group.enter()
        ws.setDefaultApplication(at: app, toOpenURLsWithScheme: "ssh") { e in
            if let e { failure = e.localizedDescription }
            group.leave()
        }
        group.notify(queue: .main) {
            note = failure.map { "Could not set the default: \($0)" } ?? "xsh is now the default terminal."
        }
    }

    private func installTool() {
        let dir = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".local/bin")
        let link = dir.appendingPathComponent("xsh")
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            if (try? FileManager.default.destinationOfSymbolicLink(atPath: link.path)) != nil {
                try FileManager.default.removeItem(at: link)
            } else if FileManager.default.fileExists(atPath: link.path) {
                note = "\(link.path) exists and is not a link. xsh did not change it."
                return
            }
            try FileManager.default.createSymbolicLink(at: link, withDestinationURL: XshCLI.binary)
            note = "Installed \(link.path). Add ~/.local/bin to your PATH if it is not there."
        } catch {
            note = error.localizedDescription
        }
    }
}

// MARK: - Themes

struct ThemesExtra: View {
    @ObservedObject private var store = Store.shared

    var body: some View {
        Section("Themes") {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 150))], spacing: 12) {
                ForEach(store.themes.indices, id: \.self) { i in
                    ThemeCard(theme: store.themes[i])
                }
            }
            .padding(.vertical, 4)
            HStack {
                Button("Import Theme…") { importTheme() }
                Button("Open Themes Folder") { openFolder() }
            }
            ForEach(store.root["theme_errors"] as? [String] ?? [], id: \.self) {
                Text($0).font(.caption).foregroundStyle(.red)
            }
        }
    }

    private func openFolder() {
        let url = URL(fileURLWithPath: store.path("themes_dir"))
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        NSWorkspace.shared.open(url)
    }

    private func importTheme() {
        let p = NSOpenPanel()
        p.allowedContentTypes = [UTType(filenameExtension: "toml") ?? .data]
        guard p.runModal() == .OK, let src = p.url else { return }
        let dir = URL(fileURLWithPath: store.path("themes_dir"))
        let dst = dir.appendingPathComponent(src.lastPathComponent)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            if FileManager.default.fileExists(atPath: dst.path) {
                store.error = "A theme named \(src.lastPathComponent) exists. Rename the file first."
                return
            }
            try FileManager.default.copyItem(at: src, to: dst)
            store.error = nil
            store.reload()
        } catch {
            store.error = error.localizedDescription
        }
    }
}

struct ThemeCard: View {
    let theme: [String: Any]
    @ObservedObject private var store = Store.shared

    private var name: String { theme["name"] as? String ?? "" }

    private func color(_ key: String, _ fallback: NSColor) -> Color {
        Color(nsColor: NSColor(hex: theme[key] as? String ?? "") ?? fallback)
    }

    var body: some View {
        Button {
            store.set("ui.theme", name)
        } label: {
            VStack(alignment: .leading, spacing: 6) {
                Text(name).font(.system(.caption, design: .monospaced)).foregroundStyle(color("foreground", .textColor))
                HStack(spacing: 2) {
                    ForEach((theme["ansi"] as? [String] ?? []).prefix(8), id: \.self) { h in
                        Rectangle()
                            .fill(Color(nsColor: NSColor(hex: h) ?? .gray))
                            .frame(width: 12, height: 8)
                    }
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, minHeight: 58, alignment: .leading)
            .background(color("background", .textBackgroundColor))
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay(
                RoundedRectangle(cornerRadius: 8).stroke(
                    store.string("ui.theme") == name ? Color.accentColor : Color.secondary.opacity(0.3),
                    lineWidth: store.string("ui.theme") == name ? 2 : 1))
        }
        .buttonStyle(.plain)
    }
}

struct AnsiExtra: View {
    @ObservedObject private var store = Store.shared
    private static let names = [
        "Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White",
        "Bright black", "Bright red", "Bright green", "Bright yellow",
        "Bright blue", "Bright magenta", "Bright cyan", "Bright white",
    ]

    private var themeAnsi: [String] {
        let name = store.string("ui.theme")
        let t = store.themes.first { $0["name"] as? String == name }
        return t?["ansi"] as? [String] ?? Array(repeating: "#808080", count: 16)
    }

    private var current: [String] {
        let own = store.strings("colors.ansi")
        return own.count == 16 ? own : themeAnsi
    }

    var body: some View {
        Section("ANSI colors") {
            LazyVGrid(columns: Array(repeating: GridItem(.flexible()), count: 8), spacing: 8) {
                ForEach(0..<16, id: \.self) { i in
                    ColorPicker("", selection: Binding {
                        Color(nsColor: NSColor(hex: current[safe: i] ?? "") ?? .gray)
                    } set: { c in
                        var list = current
                        guard list.count == 16 else { return }
                        list[i] = NSColor(c).hexString
                        store.set("colors.ansi", list)
                    }, supportsOpacity: false)
                    .labelsHidden()
                    .help(Self.names[i])
                }
            }
            if store.strings("colors.ansi").count == 16 {
                Button("Use the Theme's Colors") { store.set("colors.ansi", [String]()) }
            }
        }
    }
}

extension Array {
    subscript(safe i: Int) -> Element? { indices.contains(i) ? self[i] : nil }
}

// MARK: - Profiles

struct EditableProfile: Identifiable {
    let id = UUID()
    var name = ""
    var shell = ""
    var args = ""
    var directory = ""
    var theme = ""
    var fontFamily = ""
    var fontSize = 0
    var env = ""

    init() {}

    init(_ d: [String: Any]) {
        let p = Profile(d)
        name = p.name
        shell = p.shell
        args = p.args.joined(separator: "\n")
        directory = p.directory
        theme = p.theme
        fontFamily = p.fontFamily
        fontSize = p.fontSize
        env = p.env.joined(separator: "\n")
    }

    var plist: [String: Any] {
        let lines = { (s: String) in s.split(separator: "\n").map(String.init) }
        return [
            "name": name, "shell": shell, "args": lines(args), "directory": directory,
            "theme": theme, "font_family": fontFamily, "font_size": fontSize, "env": lines(env),
        ]
    }
}

struct ProfilesExtra: View {
    @ObservedObject private var store = Store.shared
    @State private var list: [EditableProfile] = []
    @State private var dirty = false

    var body: some View {
        Section {
            Text("A profile is a named way to start a session: a program, a folder, and a look. Use them from Shell > New Tab with Profile.")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Add Profile") {
                    var p = EditableProfile()
                    p.name = "Profile \(list.count + 1)"
                    list.append(p)
                    dirty = true
                }
                Spacer()
                Button("Save") {
                    store.saveProfiles(list.map(\.plist))
                    dirty = false
                }
                .disabled(!dirty)
                .keyboardShortcut(.defaultAction)
            }
        }
        ForEach($list) { $p in
            Section(p.name.isEmpty ? "Profile" : p.name) {
                field("Name", $p.name)
                field("Program", $p.shell, hint: "Empty uses the Terminal setting")
                area("Arguments", $p.args, hint: "One per line")
                field("Folder", $p.directory, hint: "Empty uses the normal folder")
                Picker("Theme", selection: $p.theme.onChange { dirty = true }) {
                    Text("Normal theme").tag("")
                    ForEach(store.themes.compactMap { $0["name"] as? String }, id: \.self) { Text($0).tag($0) }
                }
                Picker("Font", selection: $p.fontFamily.onChange { dirty = true }) {
                    Text("Normal font").tag("")
                    ForEach(Fonts.fixedPitchFamilies, id: \.self) { Text($0).tag($0) }
                }
                Stepper(
                    p.fontSize == 0 ? "Font size: normal" : "Font size: \(p.fontSize) pt",
                    value: $p.fontSize.onChange { dirty = true }, in: 0...96)
                area("Environment", $p.env, hint: "NAME=value, one per line")
                Button("Delete Profile", role: .destructive) {
                    list.removeAll { $0.id == p.id }
                    dirty = true
                }
            }
        }
        .onAppear { list = store.profiles.map(EditableProfile.init) }
    }

    private func field(_ label: String, _ b: Binding<String>, hint: String = "") -> some View {
        LabeledContent(label) {
            TextField("", text: b.onChange { dirty = true }, prompt: Text(hint))
                .textFieldStyle(.roundedBorder)
                .frame(maxWidth: 300)
        }
    }

    private func area(_ label: String, _ b: Binding<String>, hint: String) -> some View {
        LabeledContent(label) {
            TextField("", text: b.onChange { dirty = true }, prompt: Text(hint), axis: .vertical)
                .lineLimit(1...5)
                .textFieldStyle(.roundedBorder)
                .frame(maxWidth: 300)
        }
    }
}

extension Binding {
    func onChange(_ fn: @escaping () -> Void) -> Binding<Value> {
        Binding(get: { wrappedValue }, set: {
            wrappedValue = $0
            fn()
        })
    }
}

// MARK: - Bundles

struct BundlesExtra: View {
    @ObservedObject private var store = Store.shared
    @State private var registry: [[String: Any]] = []
    @State private var busy = false
    @State private var message = ""
    @State private var removal: String?

    var body: some View {
        Section("Installed") {
            if store.bundles.isEmpty { Text("No bundle is installed.").foregroundStyle(.secondary) }
            ForEach(store.bundles.indices, id: \.self) { i in
                installedRow(store.bundles[i])
            }
            ForEach(store.root["broken_bundles"] as? [String] ?? [], id: \.self) {
                Text($0).font(.caption).foregroundStyle(.red)
            }
            Button("Install from a Folder…") {
                let p = NSOpenPanel()
                p.canChooseFiles = false
                p.canChooseDirectories = true
                if p.runModal() == .OK, let u = p.url { run(["bundle", "install", u.path]) }
            }
        }
        Section("Registry") {
            HStack {
                Button(registry.isEmpty ? "Load the Registry" : "Refresh") { load() }
                if busy { ProgressView().controlSize(.small) }
            }
            ForEach(registry.indices, id: \.self) { i in
                let b = registry[i]
                let name = b["name"] as? String ?? ""
                LabeledContent {
                    if store.bundles.contains(where: { $0["name"] as? String == name }) {
                        Button("Update") { run(["bundle", "update", name]) }
                    } else {
                        Button("Install") { run(["bundle", "install", name]) }
                    }
                } label: {
                    Text("\(name) \(b["version"] as? String ?? "")")
                    Text(b["description"] as? String ?? "").font(.caption).foregroundStyle(.secondary)
                }
            }
            if !message.isEmpty { Text(message).font(.caption).foregroundStyle(.secondary) }
        }
        .alert("Remove this bundle?", isPresented: Binding { removal != nil } set: { if !$0 { removal = nil } }) {
            Button("Remove", role: .destructive) {
                if let removal { run(["bundle", "remove", removal]) }
                removal = nil
            }
            Button("Cancel", role: .cancel) { removal = nil }
        } message: {
            Text(removal ?? "")
        }
    }

    private func installedRow(_ b: [String: Any]) -> some View {
        let name = b["name"] as? String ?? ""
        return LabeledContent {
            HStack {
                Toggle("Enabled", isOn: Binding {
                    !store.strings("bundles.disabled").contains(name)
                } set: { on in
                    var list = store.strings("bundles.disabled").filter { $0 != name }
                    if !on { list.append(name) }
                    store.set("bundles.disabled", list)
                })
                .labelsHidden()
                Button("Remove") { removal = name }
            }
        } label: {
            Text("\(name) \(b["version"] as? String ?? "")")
            Text(b["description"] as? String ?? "").font(.caption).foregroundStyle(.secondary)
        }
    }

    private func load() {
        busy = true
        DispatchQueue.global().async {
            let r = XshCLI.run(["app", "registry"])
            let list = (try? JSONSerialization.jsonObject(with: r.out)) as? [[String: Any]]
            DispatchQueue.main.async {
                busy = false
                if let list {
                    registry = list
                    message = ""
                } else {
                    message = r.err
                }
            }
        }
    }

    private func run(_ args: [String]) {
        busy = true
        store.save(args) { r in
            busy = false
            message = (r.text + r.err).trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }
}

// MARK: - History

struct HistoryExtra: View {
    @State private var status = ""
    @State private var askClear = false

    var body: some View {
        Section("Actions") {
            HStack {
                Button("Pause") { run(["history", "pause"]) }
                Button("Resume") { run(["history", "resume"]) }
                Button("Export…") { export() }
                Spacer()
                Button("Delete All History…", role: .destructive) { askClear = true }
            }
            if !status.isEmpty { Text(status).font(.caption).foregroundStyle(.secondary) }
        }
        .alert("Delete all command history?", isPresented: $askClear) {
            Button("Delete", role: .destructive) { run(["history", "--clear"], done: "The history is empty.") }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("You cannot undo this.")
        }
    }

    private func export() {
        let p = NSSavePanel()
        p.nameFieldStringValue = "xsh-history.json"
        guard p.runModal() == .OK, let u = p.url else { return }
        run(["history", "export", "--output", u.path], done: "Saved \(u.lastPathComponent).")
    }

    private func run(_ args: [String], done: String? = nil) {
        DispatchQueue.global().async {
            let r = XshCLI.run(args)
            let text = (r.status == 0 ? (done ?? r.text) : r.err).trimmingCharacters(in: .whitespacesAndNewlines)
            DispatchQueue.main.async { status = text }
        }
    }
}

// MARK: - AI

/// What the Settings window asks for each kind of provider.
private struct AIProviderForm {
    let id: String
    let label: String
    /// Text about the key, or nil when the provider uses no key.
    let keyNote: String?
    let addressHint: String

    static let customEndpointHint = "http://127.0.0.1:9694/v1"
    static let all = [
        AIProviderForm(id: "openai", label: "OpenAI",
                       keyNote: "Required, unless OPENAI_API_KEY is set.",
                       addressHint: "https://api.openai.com/v1/"),
        AIProviderForm(id: "anthropic", label: "Anthropic",
                       keyNote: "Required, unless ANTHROPIC_API_KEY is set.",
                       addressHint: "https://api.anthropic.com/v1/"),
        AIProviderForm(id: "ollama", label: "Ollama (local)", keyNote: nil,
                       addressHint: "http://localhost:11434"),
        AIProviderForm(id: "custom", label: "Custom (OpenAI-compatible)",
                       keyNote: "Optional. Leave it empty if the service needs no key.",
                       addressHint: customEndpointHint),
    ]
}

private struct AITextField: View {
    let key: String
    let hint: String
    @ObservedObject private var store = Store.shared

    var body: some View {
        TextField("", text: Binding {
            store.string(key)
        } set: { v in
            if v.isEmpty { store.unset(key) } else { store.set(key, v) }
        }, prompt: Text(hint))
        .textFieldStyle(.roundedBorder)
        .frame(maxWidth: 300)
        .id(key)
    }
}

struct AIKeysExtra: View {
    @ObservedObject private var store = Store.shared
    @State private var keys: [String: String] = [:]

    private var form: AIProviderForm? {
        AIProviderForm.all.first { $0.id == store.string("ai.default.provider") }
    }

    var body: some View {
        Section("AI provider") {
            Picker("Provider", selection: Binding {
                store.string("ai.default.provider")
            } set: { store.set("ai.default.provider", $0) }) {
                ForEach(AIProviderForm.all, id: \.id) { Text($0.label).tag($0.id) }
            }
            if let form {
                fields(form)
            }
            if let e = store.ai["error"] as? String { Text(e).font(.caption).foregroundStyle(.red) }
        }
    }

    @ViewBuilder private func fields(_ form: AIProviderForm) -> some View {
        if let note = form.keyNote {
            keyRow(form.id)
            Text("\(note) Keys go to the macOS Keychain. They are not in any file.")
                .font(.caption).foregroundStyle(.secondary)
        } else {
            Text("This provider needs no API key.").font(.caption).foregroundStyle(.secondary)
        }
        if form.id == "custom" {
            AICustomModelRows()
        } else {
            LabeledContent("Model") {
                let fallback = store.string("ai.default.model")
                AITextField(key: "ai.providers.\(form.id).model", hint: fallback.isEmpty ? "Default" : fallback)
            }
            LabeledContent("Address") {
                AITextField(key: "ai.providers.\(form.id).base_url", hint: form.addressHint)
            }
        }
    }

    private func keyRow(_ name: String) -> some View {
        let state = store.string("ai.providers.\(name).key")
        return LabeledContent("API key") {
            HStack {
                SecureField("", text: Binding { keys[name] ?? "" } set: { keys[name] = $0 },
                            prompt: Text(state.isEmpty ? "Paste the key" : state == "keychain" ? "Saved in Keychain" : state))
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 240)
                Button("Save") {
                    store.save(["app", "ai-key", name], stdin: Data((keys[name] ?? "").utf8))
                    keys[name] = ""
                }
                .disabled((keys[name] ?? "").isEmpty)
                if state == "keychain" {
                    Button("Remove") { store.save(["app", "ai-key", name], stdin: Data()) }
                }
            }
        }
        .id(name)
    }
}

private struct AICustomModelRows: View {
    private static let modelKey = "ai.providers.custom.model"

    @ObservedObject private var store = Store.shared
    @State private var models: [String] = []
    @State private var fetching = false
    @State private var failure = ""

    private var url: String {
        store.string("ai.providers.custom.base_url").trimmingCharacters(in: .whitespaces)
    }

    var body: some View {
        LabeledContent("Endpoint") {
            HStack {
                AITextField(key: "ai.providers.custom.base_url", hint: AIProviderForm.customEndpointHint)
                if fetching { ProgressView().controlSize(.small) }
                Button("Fetch Models", action: fetch).disabled(url.isEmpty || fetching)
            }
        }
        .onChange(of: url) {
            models = []
            failure = ""
        }
        if !failure.isEmpty {
            Text(failure).font(.caption).foregroundStyle(.red)
        }
        if models.isEmpty {
            LabeledContent("Model") {
                AITextField(key: Self.modelKey, hint: "Fetch models or type a name")
            }
        } else {
            Picker("Model", selection: Binding {
                store.string(Self.modelKey)
            } set: { store.set(Self.modelKey, $0) }) {
                let current = store.string(Self.modelKey)
                if !models.contains(current) {
                    Text(current.isEmpty ? "Select a model" : current).tag(current)
                }
                ForEach(models, id: \.self) { Text($0).tag($0) }
            }
        }
    }

    private func fetch() {
        let address = url
        fetching = true
        failure = ""
        DispatchQueue.global().async {
            let r = XshCLI.run(["app", "ai-models", address])
            let list = r.status == 0 ? (try? JSONSerialization.jsonObject(with: r.out)) as? [String] : nil
            let text = r.err.trimmingCharacters(in: .whitespacesAndNewlines)
            DispatchQueue.main.async {
                fetching = false
                if let list {
                    models = list
                } else {
                    failure = text.isEmpty ? "Cannot read the model list." : text
                }
            }
        }
    }
}

// MARK: - Advanced

struct AdvancedExtra: View {
    @ObservedObject private var store = Store.shared
    @State private var filter = ""
    @State private var askReset = false

    var body: some View {
        Section("Files") {
            LabeledContent("Settings file") { open("config_file") }
            LabeledContent("AI settings file") { open("ai_file") }
            LabeledContent("Start-up script") { open("rc_file") }
            LabeledContent("Config folder") {
                Button("Show in Finder") { NSWorkspace.shared.open(URL(fileURLWithPath: store.path("config_dir"))) }
            }
            LabeledContent("Data folder") {
                Button("Show in Finder") { NSWorkspace.shared.open(URL(fileURLWithPath: store.path("data_dir"))) }
            }
        }
        Section("All settings") {
            TextField("", text: $filter, prompt: Text("Filter"))
                .textFieldStyle(.roundedBorder)
            ForEach(store.allKeys.filter { filter.isEmpty || $0.contains(filter.lowercased()) }, id: \.self) { key in
                RawRow(key: key)
            }
        }
        Section("Reset") {
            Button("Reset All Settings…", role: .destructive) { askReset = true }
            Text("xsh keeps the old file as config.toml.bak.").font(.caption).foregroundStyle(.secondary)
            Text("xsh \(store.version)").font(.caption).foregroundStyle(.secondary)
        }
        .alert("Reset all settings to their defaults?", isPresented: $askReset) {
            Button("Reset", role: .destructive) { reset() }
            Button("Cancel", role: .cancel) {}
        }
    }

    private func open(_ name: String) -> some View {
        Button("Open") {
            let path = store.path(name)
            if !FileManager.default.fileExists(atPath: path) {
                try? FileManager.default.createDirectory(
                    at: URL(fileURLWithPath: path).deletingLastPathComponent(), withIntermediateDirectories: true)
                FileManager.default.createFile(atPath: path, contents: Data())
            }
            NSWorkspace.shared.open([URL(fileURLWithPath: path)],
                                    withApplicationAt: URL(fileURLWithPath: "/System/Applications/TextEdit.app"),
                                    configuration: NSWorkspace.OpenConfiguration())
        }
    }

    private func reset() {
        let file = store.path("config_file")
        let bak = file + ".bak"
        do {
            try? FileManager.default.removeItem(atPath: bak)
            if FileManager.default.fileExists(atPath: file) {
                try FileManager.default.moveItem(atPath: file, toPath: bak)
            }
            store.error = nil
            store.reload()
        } catch {
            store.error = error.localizedDescription
        }
    }
}

/// One setting as raw TOML; saved when you press Return.
struct RawRow: View {
    let key: String
    @ObservedObject private var store = Store.shared
    @State private var text = ""

    private var saved: String {
        guard let v = store.value(key) else { return "" }
        if let b = v as? Bool { return b ? "true" : "false" }
        if let n = v as? NSNumber { return n.stringValue }
        let opts: JSONSerialization.WritingOptions = [.fragmentsAllowed, .withoutEscapingSlashes]
        return (try? JSONSerialization.data(withJSONObject: v, options: opts)).map { String(decoding: $0, as: UTF8.self) } ?? ""
    }

    var body: some View {
        LabeledContent(key) {
            TextField("", text: $text)
                .textFieldStyle(.roundedBorder)
                .font(.system(.body, design: .monospaced))
                .frame(maxWidth: 320)
                .onSubmit { store.save(["config", "set", key, text]) }
        }
        .onAppear { text = saved }
        .onChange(of: saved) { _, new in text = new }
    }
}
