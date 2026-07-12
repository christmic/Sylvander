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
}
