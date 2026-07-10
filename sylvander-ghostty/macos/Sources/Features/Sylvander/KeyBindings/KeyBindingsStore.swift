// KeyBindingsStore.swift
// 持久化快捷键到 ~/Library/Application Support/Sylvander/keybindings.json。
// F2 默认:⇧⌘T = new_sylvander_tab。可在 Preferences 里改。

import Foundation
import AppKit

struct KeyCombo: Codable, Equatable, Hashable {
    var keyCode: UInt16       // virtual key code
    var modifiers: UInt      // NSEvent.ModifierFlags rawValue

    var displayString: String {
        var parts: [String] = []
        let mods = NSEvent.ModifierFlags(rawValue: modifiers)
        if mods.contains(.control) { parts.append("⌃") }
        if mods.contains(.option) { parts.append("⌥") }
        if mods.contains(.shift) { parts.append("⇧") }
        if mods.contains(.command) { parts.append("⌘") }
        if let ch = keyCodeToChar() {
            parts.append(ch.uppercased())
        }
        return parts.joined()
    }

    static let defaultNewTab = KeyCombo(
        keyCode: 17,  // 't'
        modifiers: NSEvent.ModifierFlags([.command, .shift]).rawValue
    )

    private func keyCodeToChar() -> String? {
        // 简化映射:F2 阶段只支持 t/w/n 等几个
        switch keyCode {
        case 17: return "t"
        case 13: return "w"
        case 45: return "n"
        case 49: return "space"
        case 53: return "esc"
        default: return nil
        }
    }
}

@MainActor
final class KeyBindingsStore: ObservableObject {
    @Published var newSylvanderTab: KeyCombo = .defaultNewTab

    private var fileURL: URL {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!
            .appendingPathComponent("Sylvander", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("keybindings.json")
    }

    func load() {
        guard let data = try? Data(contentsOf: fileURL),
              let decoded = try? JSONDecoder().decode(Persisted.self, from: data) else { return }
        newSylvanderTab = decoded.newSylvanderTab
    }

    func save() {
        let persisted = Persisted(newSylvanderTab: newSylvanderTab)
        if let data = try? JSONEncoder().encode(persisted) {
            try? data.write(to: fileURL, options: .atomic)
        }
    }

    private struct Persisted: Codable {
        var newSylvanderTab: KeyCombo
    }
}