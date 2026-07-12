import Foundation

enum Fmt {
    /// Compact token count: 1234 -> "1.2K", 3_400_000 -> "3.4M".
    static func tokens(_ n: Int64) -> String {
        let v = Double(n)
        switch abs(n) {
        case 1_000_000_000...:
            return trim(v / 1_000_000_000) + "B"
        case 1_000_000...:
            return trim(v / 1_000_000) + "M"
        case 1_000...:
            return trim(v / 1_000) + "K"
        default:
            return "\(n)"
        }
    }

    private static func trim(_ v: Double) -> String {
        let s = String(format: "%.1f", v)
        return s.hasSuffix(".0") ? String(s.dropLast(2)) : s
    }

    static func pct(_ ratio: Double) -> Double { (ratio * 100).rounded() }

    /// Whole-number percent from a 0-100 value, e.g. 63.7 -> "64%".
    static func percent(_ v: Double) -> String {
        "\(Int(v.rounded()))%"
    }

    /// Date key "yyyy-MM-dd" rendered with a Gregorian calendar so the
    /// output is stable regardless of the system calendar. Used both
    /// for outgoing range queries and for matching incoming API dates.
    static func dateKey(_ d: Date, calendar: Calendar = .current) -> String {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.timeZone = calendar.timeZone
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: d)
    }

    /// Parse a YYYY-MM-DD API string into a local-time Date.
    /// Per checklist §4 B2: never use an implicit locale-dependent
    /// formatter. We always use the Gregorian calendar with POSIX
    /// locale so the date round-trips losslessly.
    static func parseDateKey(_ key: String, calendar: Calendar = .current) -> Date? {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.timeZone = calendar.timeZone
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f.date(from: key)
    }

    /// Iterate every calendar day from `from` to `to` inclusive, in the
    /// given calendar's timezone. Used to fill missing days so the
    /// heatmap and daily aggregation don't have holes.
    static func enumerateDays(from: Date, to: Date, calendar: Calendar = .current) -> [Date] {
        var cal = calendar
        cal.timeZone = TimeZone.current
        let f = DateFormatter()
        f.calendar = cal
        f.timeZone = cal.timeZone
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        let fromK = f.string(from: from)
        let toK = f.string(from: to)
        var out: [Date] = []
        var d = from
        while f.string(from: d) <= toK {
            out.append(d)
            guard let next = cal.date(byAdding: .day, value: 1, to: d) else { break }
            d = next
            if f.string(from: d) < fromK { break }   // safety net
        }
        return out
    }

    /// Localized short date for tooltips, e.g. "7月10日".
    static func shortDate(_ d: Date, calendar: Calendar = .current) -> String {
        let f = DateFormatter()
        f.calendar = calendar
        f.timeZone = calendar.timeZone
        f.locale = Locale(identifier: "zh_CN")
        f.dateFormat = "M月d日"
        return f.string(from: d)
    }
}