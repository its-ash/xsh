import AppKit

/// The menu bar. Shortcuts come from the `[keys]` settings.
enum MainMenu {
    private static func item(_ title: String, _ action: Selector?, key: String? = nil, tag: Int = 0) -> NSMenuItem {
        let i = NSMenuItem(title: title, action: action, keyEquivalent: "")
        i.tag = tag
        if let key, let s = Shortcut(Store.shared.string("keys.\(key)")) {
            let (text, mods) = s.menu
            i.keyEquivalent = text
            i.keyEquivalentModifierMask = mods
        }
        return i
    }

    private static func fixed(_ title: String, _ action: Selector?, _ key: String, _ mods: NSEvent.ModifierFlags = .command) -> NSMenuItem {
        let i = NSMenuItem(title: title, action: action, keyEquivalent: key)
        i.keyEquivalentModifierMask = mods
        return i
    }

    private static func top(_ title: String, _ items: [NSMenuItem]) -> NSMenuItem {
        let holder = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        let m = NSMenu(title: title)
        items.forEach(m.addItem)
        holder.submenu = m
        return holder
    }

    static func build(delegate d: AppDelegate, profiles: [String]) -> NSMenu {
        let bar = NSMenu()
        let w = TerminalWindow.self
        let sep = { NSMenuItem.separator() }

        let settings = item("Settings…", #selector(AppDelegate.showSettings(_:)), key: "settings")
        settings.target = d
        let services = NSMenuItem(title: "Services", action: nil, keyEquivalent: "")
        let servicesMenu = NSMenu(title: "Services")
        services.submenu = servicesMenu
        NSApp.servicesMenu = servicesMenu
        let updates = NSMenuItem(title: "Check for Updates…", action: #selector(AppDelegate.checkForUpdates(_:)), keyEquivalent: "")
        updates.target = d
        bar.addItem(top("xsh", [
            NSMenuItem(title: "About xsh", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: ""),
            updates,
            sep(), settings, sep(), services, sep(),
            fixed("Hide xsh", #selector(NSApplication.hide(_:)), "h"),
            fixed("Hide Others", #selector(NSApplication.hideOtherApplications(_:)), "h", [.command, .option]),
            NSMenuItem(title: "Show All", action: #selector(NSApplication.unhideAllApplications(_:)), keyEquivalent: ""),
            sep(),
            fixed("Quit xsh", #selector(NSApplication.terminate(_:)), "q"),
        ]))

        let newWindow = item("New Window", #selector(AppDelegate.newWindow(_:)), key: "new_window")
        let newTab = item("New Tab", #selector(AppDelegate.newTab(_:)), key: "new_tab")
        newWindow.target = d
        newTab.target = d
        var shell = [newWindow, newTab]
        if !profiles.isEmpty {
            let list = { (title: String, action: Selector) -> NSMenuItem in
                top(title, profiles.map { name in
                    let i = NSMenuItem(title: name, action: action, keyEquivalent: "")
                    i.target = d
                    i.representedObject = name
                    return i
                })
            }
            shell.append(list("New Tab with Profile", #selector(AppDelegate.newTabWithProfile(_:))))
            shell.append(list("New Window with Profile", #selector(AppDelegate.newWindowWithProfile(_:))))
        }
        shell += [
            sep(),
            item("Split Right", #selector(w.splitRight(_:)), key: "split_right"),
            item("Split Down", #selector(w.splitDown(_:)), key: "split_down"),
            item("Make Panes Equal", #selector(w.equalizePanes(_:)), key: "equalize_panes"),
            sep(),
            item("Close", #selector(w.closePane(_:)), key: "close"),
        ]
        bar.addItem(top("Shell", shell))

        let findPanel = NSSelectorFromString("performFindPanelAction:")
        bar.addItem(top("Edit", [
            fixed("Copy", #selector(NSText.copy(_:)), "c"),
            fixed("Paste", #selector(NSText.paste(_:)), "v"),
            fixed("Select All", #selector(NSText.selectAll(_:)), "a"),
            sep(),
            item("Find…", findPanel, key: "find", tag: Int(NSFindPanelAction.showFindPanel.rawValue)),
            item("Find Next", findPanel, key: "find_next", tag: Int(NSFindPanelAction.next.rawValue)),
            item("Find Previous", findPanel, key: "find_previous", tag: Int(NSFindPanelAction.previous.rawValue)),
            sep(),
            item("Clear Scrollback", #selector(w.clearScrollback(_:)), key: "clear_scrollback"),
            item("Reset Terminal", #selector(w.resetTerminal(_:)), key: "reset"),
        ]))

        bar.addItem(top("View", [
            item("Bigger Text", #selector(w.biggerText(_:)), key: "font_bigger"),
            item("Smaller Text", #selector(w.smallerText(_:)), key: "font_smaller"),
            item("Default Text Size", #selector(w.defaultText(_:)), key: "font_reset"),
            sep(),
            item("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), key: "fullscreen"),
        ]))

        let windowMenu = top("Window", [
            fixed("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"),
            NSMenuItem(title: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: ""),
            sep(),
            item("Next Tab", #selector(NSWindow.selectNextTab(_:)), key: "next_tab"),
            item("Previous Tab", #selector(NSWindow.selectPreviousTab(_:)), key: "previous_tab"),
            sep(),
            item("Next Pane", #selector(w.focusNextPane(_:)), key: "next_pane"),
            item("Previous Pane", #selector(w.focusPreviousPane(_:)), key: "previous_pane"),
            item("Pane to the Left", #selector(w.focusLeft(_:)), key: "focus_left"),
            item("Pane to the Right", #selector(w.focusRight(_:)), key: "focus_right"),
            item("Pane Above", #selector(w.focusUp(_:)), key: "focus_up"),
            item("Pane Below", #selector(w.focusDown(_:)), key: "focus_down"),
        ])
        bar.addItem(windowMenu)
        NSApp.windowsMenu = windowMenu.submenu

        let help = item("xsh Help", #selector(AppDelegate.showHelp(_:)))
        help.target = d
        let helpMenu = top("Help", [help])
        bar.addItem(helpMenu)
        NSApp.helpMenu = helpMenu.submenu
        return bar
    }
}
