import AppKit
import SwiftTerm

extension NSColor {
    /// `#rrggbb`, or nil for anything else.
    convenience init?(hex: String) {
        guard hex.count == 7, hex.first == "#", let v = UInt32(hex.dropFirst(), radix: 16) else { return nil }
        self.init(
            srgbRed: CGFloat((v >> 16) & 0xff) / 255,
            green: CGFloat((v >> 8) & 0xff) / 255,
            blue: CGFloat(v & 0xff) / 255,
            alpha: 1)
    }

    var hexString: String {
        let c = usingColorSpace(.sRGB) ?? self
        let n = { (x: CGFloat) in Int((min(max(x, 0), 1) * 255).rounded()) }
        return String(format: "#%02x%02x%02x", n(c.redComponent), n(c.greenComponent), n(c.blueComponent))
    }
}

/// A session's way to start and look, from the settings and a profile.
struct Profile {
    var name = ""
    var shell = ""
    var args: [String] = []
    var directory = ""
    var theme = ""
    var fontFamily = ""
    var fontSize = 0
    var env: [String] = []

    init() {}

    init(_ d: [String: Any]) {
        name = d["name"] as? String ?? ""
        shell = d["shell"] as? String ?? ""
        args = d["args"] as? [String] ?? []
        directory = d["directory"] as? String ?? ""
        theme = d["theme"] as? String ?? ""
        fontFamily = d["font_family"] as? String ?? ""
        fontSize = d["font_size"] as? Int ?? 0
        env = d["env"] as? [String] ?? []
    }
}

struct Palette {
    var background: NSColor
    var foreground: NSColor
    var cursor: NSColor
    var selection: NSColor
    var ansi: [Color]?

    /// The theme for the current light or dark mode, with `[colors]` on top.
    static func current(profile: Profile?) -> Palette {
        let s = Store.shared
        let dark = NSApp.effectiveAppearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        let auto = s.string(dark ? "window.dark_theme" : "window.light_theme")
        let name = [profile?.theme ?? "", auto, s.string("ui.theme")].first { !$0.isEmpty } ?? ""
        let t = s.themes.first { $0["name"] as? String == name } ?? [:]
        func pick(_ key: String) -> NSColor? {
            NSColor(hex: s.string("colors.\(key)")) ?? NSColor(hex: t[key] as? String ?? "")
        }
        let bg = pick("background") ?? .textBackgroundColor
        let fg = pick("foreground") ?? .textColor
        let own = s.strings("colors.ansi")
        let list = own.count == 16 ? own : (t["ansi"] as? [String] ?? [])
        let ansi = list.count == 16
            ? list.compactMap { NSColor(hex: $0).map { Color(nsColor: $0) } }
            : nil
        return Palette(
            background: bg,
            foreground: fg,
            cursor: pick("cursor") ?? fg,
            selection: pick("selection") ?? .selectedTextBackgroundColor,
            ansi: ansi?.count == 16 ? ansi : nil)
    }
}

enum Fonts {
    /// Size change from the View menu, for this run of the app only.
    static var delta = 0

    static func make(profile: Profile?) -> NSFont {
        let s = Store.shared
        let family = [profile?.fontFamily ?? "", s.string("font.family")].first { !$0.isEmpty } ?? ""
        let base = (profile?.fontSize ?? 0) > 0 ? profile?.fontSize ?? 13 : s.int("font.size")
        let size = CGFloat(min(max(base + delta, 6), 96))
        // An empty family name would give a proportional default font.
        guard !family.isEmpty else { return .monospacedSystemFont(ofSize: size, weight: .regular) }
        return NSFontManager.shared.font(withFamily: family, traits: [], weight: 5, size: size)
            ?? .monospacedSystemFont(ofSize: size, weight: .regular)
    }

    static var fixedPitchFamilies: [String] {
        let names = NSFontManager.shared.availableFontNames(with: .fixedPitchFontMask) ?? []
        let families = names.compactMap { NSFont(name: $0, size: 12)?.familyName }
        return Array(Set(families)).filter { !$0.hasPrefix(".") }.sorted()
    }

    /// Size of one cell, used to size a new window.
    static func cell(_ font: NSFont, lineSpacing: CGFloat) -> NSSize {
        let w = ("M" as NSString).size(withAttributes: [.font: font]).width
        let h = (font.ascender - font.descender + font.leading) * max(lineSpacing, 1)
        return NSSize(width: ceil(w), height: ceil(h))
    }
}

/// A word safe to give to any POSIX shell.
func shellQuote(_ s: String) -> String {
    "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
}
