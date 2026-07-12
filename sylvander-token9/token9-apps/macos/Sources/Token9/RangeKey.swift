import Foundation

/// Time range presets for the dashboard. `range()` returns inclusive
/// (from, to) as YYYY-MM-DD in local time for the /stats/summary query.
enum RangeKey: String, CaseIterable, Identifiable {
    case yesterday, today, week, lastWeek, month, year
    var id: String { rawValue }

    var label: String {
        switch self {
        case .yesterday: return "昨日"
        case .today: return "今日"
        case .week: return "本周"
        case .lastWeek: return "上周"
        case .month: return "本月"
        case .year: return "本年"
        }
    }

    func range(now: Date = Date(), calendar: Calendar = .current) -> (from: String, to: String) {
        let day = 60 * 60 * 24.0
        switch self {
        case .today:
            return (fmt(now), fmt(now))
        case .yesterday:
            let y = now.addingTimeInterval(-day)
            return (fmt(y), fmt(y))
        case .week:
            let start = startOfWeek(now, calendar)
            return (fmt(start), fmt(now))
        case .lastWeek:
            let thisStart = startOfWeek(now, calendar)
            let lastStart = thisStart.addingTimeInterval(-7 * day)
            let lastEnd = thisStart.addingTimeInterval(-day)
            return (fmt(lastStart), fmt(lastEnd))
        case .month:
            let comps = calendar.dateComponents([.year, .month], from: now)
            let start = calendar.date(from: comps) ?? now
            return (fmt(start), fmt(now))
        case .year:
            let comps = calendar.dateComponents([.year], from: now)
            let start = calendar.date(from: comps) ?? now
            return (fmt(start), fmt(now))
        }
    }

    private func startOfWeek(_ date: Date, _ calendar: Calendar) -> Date {
        calendar.dateInterval(of: .weekOfYear, for: date)?.start ?? date
    }

    private func fmt(_ date: Date) -> String {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: date)
    }
}
