import AppKit
import Carbon.HIToolbox

/// A shortcut as the settings store it: `cmd+shift+d`.
struct Shortcut {
    var key: String
    var modifiers: NSEvent.ModifierFlags

    private static let named: [String: Int] = [
        "left": NSLeftArrowFunctionKey, "right": NSRightArrowFunctionKey,
        "up": NSUpArrowFunctionKey, "down": NSDownArrowFunctionKey,
        "home": NSHomeFunctionKey, "end": NSEndFunctionKey,
        "pageup": NSPageUpFunctionKey, "pagedown": NSPageDownFunctionKey,
    ]
    private static let shifted: [Character: Character] = [
        "]": "}", "[": "{", "=": "+", "-": "_", ";": ":", "'": "\"", ",": "<", ".": ">", "/": "?", "\\": "|",
        "`": "~", "1": "!", "2": "@", "3": "#", "4": "$", "5": "%", "6": "^", "7": "&", "8": "*", "9": "(", "0": ")",
    ]

    init?(_ text: String) {
        // `+` is both the separator and a possible key.
        var parts = text.lowercased().split(separator: "+", omittingEmptySubsequences: false).map(String.init)
        if text.hasSuffix("+") { parts = parts.dropLast(2) + ["+"] }
        guard let last = parts.popLast(), !last.isEmpty else { return nil }
        key = last
        modifiers = []
        for m in parts {
            switch m {
            case "cmd", "command": modifiers.insert(.command)
            case "shift": modifiers.insert(.shift)
            case "alt", "option", "opt": modifiers.insert(.option)
            case "ctrl", "control": modifiers.insert(.control)
            default: return nil
            }
        }
    }

    /// Text and modifiers for a menu item.
    var menu: (String, NSEvent.ModifierFlags) {
        if let code = Self.named[key], let u = UnicodeScalar(code) {
            return (String(Character(u)), modifiers)
        }
        switch key {
        case "enter", "return": return ("\r", modifiers)
        case "tab": return ("\t", modifiers)
        case "space": return (" ", modifiers)
        case "esc", "escape": return ("\u{1b}", modifiers)
        case "delete", "backspace": return ("\u{8}", modifiers)
        default: break
        }
        if key.hasPrefix("f"), let n = Int(key.dropFirst()), (1...20).contains(n), let u = UnicodeScalar(NSF1FunctionKey + n - 1) {
            return (String(Character(u)), modifiers)
        }
        // AppKit matches the shifted character of a symbol key.
        if modifiers.contains(.shift), key.count == 1, let c = key.first, let up = Self.shifted[c] {
            return (String(up), modifiers.subtracting(.shift))
        }
        return (key, modifiers)
    }

    /// The text for the settings from a key press; nil without a modifier.
    static func text(from e: NSEvent) -> String? {
        let flags = e.modifierFlags.intersection(.deviceIndependentFlagsMask)
        guard !flags.intersection([.command, .control, .option]).isEmpty || (e.specialKey != nil) else { return nil }
        var parts: [String] = []
        if flags.contains(.command) { parts.append("cmd") }
        if flags.contains(.control) { parts.append("ctrl") }
        if flags.contains(.option) { parts.append("alt") }
        if flags.contains(.shift) { parts.append("shift") }
        guard let key = keyName(e) else { return nil }
        parts.append(key)
        return parts.joined(separator: "+")
    }

    private static func keyName(_ e: NSEvent) -> String? {
        switch Int(e.keyCode) {
        case kVK_LeftArrow: return "left"
        case kVK_RightArrow: return "right"
        case kVK_UpArrow: return "up"
        case kVK_DownArrow: return "down"
        case kVK_Space: return "space"
        case kVK_Return: return "enter"
        case kVK_Tab: return "tab"
        default: break
        }
        if let f = fKeys[Int(e.keyCode)] { return f }
        // Option changes the character (⌥D gives ∂), so read the plain key.
        guard let c = e.characters(byApplyingModifiers: [])?.lowercased(), c.count == 1 else { return nil }
        return c
    }

    private static let fKeys: [Int: String] = [
        kVK_F1: "f1", kVK_F2: "f2", kVK_F3: "f3", kVK_F4: "f4", kVK_F5: "f5", kVK_F6: "f6",
        kVK_F7: "f7", kVK_F8: "f8", kVK_F9: "f9", kVK_F10: "f10", kVK_F11: "f11", kVK_F12: "f12",
    ]

    /// Symbols such as `⌘⇧D`, for the settings.
    static func display(_ text: String) -> String {
        guard let s = Shortcut(text) else { return text.isEmpty ? "None" : text }
        var out = ""
        if s.modifiers.contains(.control) { out += "⌃" }
        if s.modifiers.contains(.option) { out += "⌥" }
        if s.modifiers.contains(.shift) { out += "⇧" }
        if s.modifiers.contains(.command) { out += "⌘" }
        let names = ["left": "←", "right": "→", "up": "↑", "down": "↓", "enter": "↩", "tab": "⇥", "space": "Space"]
        return out + (names[s.key] ?? s.key.uppercased())
    }

    // MARK: Global hotkey

    /// The Carbon key code; the global hotkey API has no other input.
    var carbonCode: UInt32? {
        let map: [String: Int] = [
            "a": kVK_ANSI_A, "b": kVK_ANSI_B, "c": kVK_ANSI_C, "d": kVK_ANSI_D, "e": kVK_ANSI_E, "f": kVK_ANSI_F,
            "g": kVK_ANSI_G, "h": kVK_ANSI_H, "i": kVK_ANSI_I, "j": kVK_ANSI_J, "k": kVK_ANSI_K, "l": kVK_ANSI_L,
            "m": kVK_ANSI_M, "n": kVK_ANSI_N, "o": kVK_ANSI_O, "p": kVK_ANSI_P, "q": kVK_ANSI_Q, "r": kVK_ANSI_R,
            "s": kVK_ANSI_S, "t": kVK_ANSI_T, "u": kVK_ANSI_U, "v": kVK_ANSI_V, "w": kVK_ANSI_W, "x": kVK_ANSI_X,
            "y": kVK_ANSI_Y, "z": kVK_ANSI_Z, "0": kVK_ANSI_0, "1": kVK_ANSI_1, "2": kVK_ANSI_2, "3": kVK_ANSI_3,
            "4": kVK_ANSI_4, "5": kVK_ANSI_5, "6": kVK_ANSI_6, "7": kVK_ANSI_7, "8": kVK_ANSI_8, "9": kVK_ANSI_9,
            "`": kVK_ANSI_Grave, "-": kVK_ANSI_Minus, "=": kVK_ANSI_Equal, "[": kVK_ANSI_LeftBracket,
            "]": kVK_ANSI_RightBracket, ";": kVK_ANSI_Semicolon, "'": kVK_ANSI_Quote, ",": kVK_ANSI_Comma,
            ".": kVK_ANSI_Period, "/": kVK_ANSI_Slash, "\\": kVK_ANSI_Backslash, "space": kVK_Space,
            "enter": kVK_Return, "tab": kVK_Tab, "left": kVK_LeftArrow, "right": kVK_RightArrow, "up": kVK_UpArrow,
            "down": kVK_DownArrow,
        ]
        let f = Self.fKeys.first { $0.value == key }?.key
        return (map[key] ?? f).map(UInt32.init)
    }

    var carbonModifiers: UInt32 {
        var m = 0
        if modifiers.contains(.command) { m |= cmdKey }
        if modifiers.contains(.shift) { m |= shiftKey }
        if modifiers.contains(.option) { m |= optionKey }
        if modifiers.contains(.control) { m |= controlKey }
        return UInt32(m)
    }
}

/// Shows or hides the app from any other app.
enum GlobalHotkey {
    private static var ref: EventHotKeyRef?
    private static var current = ""
    private static var handler: EventHandlerRef?

    static func update() {
        let text = Store.shared.string("keys.global_hotkey")
        guard text != current else { return }
        current = text
        if let ref { UnregisterEventHotKey(ref) }
        ref = nil
        guard let s = Shortcut(text), let code = s.carbonCode else { return }
        if handler == nil {
            var type = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
            InstallEventHandler(GetApplicationEventTarget(), { _, _, _ in
                DispatchQueue.main.async { GlobalHotkey.toggle() }
                return noErr
            }, 1, &type, nil, &handler)
        }
        let id = EventHotKeyID(signature: OSType(0x7873_6868), id: 1)
        RegisterEventHotKey(code, s.carbonModifiers, id, GetApplicationEventTarget(), 0, &ref)
    }

    private static func toggle() {
        if NSApp.isActive, NSApp.keyWindow != nil {
            NSApp.hide(nil)
        } else {
            NSApp.activate(ignoringOtherApps: true)
            if TerminalWindow.all.isEmpty { Terminals.newWindow(profile: nil) }
        }
    }
}
