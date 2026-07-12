import XCTest
@testable import Token9

/// RangeKey behavior tests. No SwiftUI dependencies.
final class RangeKeyTests: XCTestCase {

    func test_showsHeatmap_trueForMultiDayRanges() {
        XCTAssertTrue(RangeKey.week.showsHeatmap)
        XCTAssertTrue(RangeKey.lastWeek.showsHeatmap)
        XCTAssertTrue(RangeKey.month.showsHeatmap)
        XCTAssertTrue(RangeKey.year.showsHeatmap)
    }

    func test_showsHeatmap_trueForSingleDayRanges() {
        XCTAssertTrue(RangeKey.yesterday.showsHeatmap)
        XCTAssertTrue(RangeKey.today.showsHeatmap)
    }

    func test_yearRange_isJanFirstThroughToday() {
        let cal = Calendar(identifier: .gregorian)
        let now = cal.date(from: DateComponents(year: 2026, month: 7, day: 12))!
        let (from, to) = RangeKey.year.range(now: now, calendar: cal)
        XCTAssertEqual(from, "2026-01-01")
        XCTAssertEqual(to, "2026-07-12")
    }

    func test_heatmapTitle_usesRollingYearWindow() {
        let cal = Calendar(identifier: .gregorian)
        let now = cal.date(from: DateComponents(year: 2026, month: 7, day: 12))!
        let title = RangeKey.month.heatmapTitle(now: now, calendar: cal)
        XCTAssertEqual(title, "2025.08—2026.07")
    }

    func test_heatmapTitle_yearIsFourDigits() {
        let cal = Calendar(identifier: .gregorian)
        let now = cal.date(from: DateComponents(year: 2026, month: 7, day: 12))!
        XCTAssertEqual(RangeKey.year.heatmapTitle(now: now, calendar: cal), "2025.08—2026.07")
    }

    func test_heatmapRange_isTwelveCalendarMonths() {
        let cal = Calendar(identifier: .gregorian)
        let now = cal.date(from: DateComponents(year: 2026, month: 7, day: 12))!
        let window = RangeKey.today.heatmapRange(now: now, calendar: cal)
        XCTAssertEqual(window.from, "2025-08-01")
        XCTAssertEqual(window.to, "2026-07-12")
    }
}
