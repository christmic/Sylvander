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

    func test_showsHeatmap_falseForSingleDayRanges() {
        XCTAssertFalse(RangeKey.yesterday.showsHeatmap)
        XCTAssertFalse(RangeKey.today.showsHeatmap)
    }

    func test_yearRange_isJanFirstThroughToday() {
        let cal = Calendar(identifier: .gregorian)
        let now = cal.date(from: DateComponents(year: 2026, month: 7, day: 12))!
        let (from, to) = RangeKey.year.range(now: now, calendar: cal)
        XCTAssertEqual(from, "2026-01-01")
        XCTAssertEqual(to, "2026-07-12")
    }

    func test_heatmapTitle_monthUsesLocalizedChineseLabel() {
        let cal = Calendar(identifier: .gregorian)
        let now = cal.date(from: DateComponents(year: 2026, month: 7, day: 12))!
        let title = RangeKey.month.heatmapTitle(now: now, calendar: cal)
        XCTAssertEqual(title, "2026年7月")
    }

    func test_heatmapTitle_yearIsFourDigits() {
        let cal = Calendar(identifier: .gregorian)
        let now = cal.date(from: DateComponents(year: 2026, month: 7, day: 12))!
        XCTAssertEqual(RangeKey.year.heatmapTitle(now: now, calendar: cal), "2026")
    }

    func test_heatmapTitle_singleDayIsEmpty() {
        XCTAssertEqual(RangeKey.today.heatmapTitle(), "")
        XCTAssertEqual(RangeKey.yesterday.heatmapTitle(), "")
    }
}