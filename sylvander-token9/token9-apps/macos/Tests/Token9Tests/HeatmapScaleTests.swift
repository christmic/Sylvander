import XCTest
@testable import Token9

/// Tests for the pure `HeatmapScale` function and the calendar-fill
/// helper. No SwiftUI dependencies.
final class HeatmapScaleTests: XCTestCase {

    func test_emptyInput_allLevelZero() {
        let result = HeatmapScale.level(tokens: 0, allNonZero: [])
        XCTAssertEqual(result, 0)
    }

    func test_equalValues_allLevelThree_noCrash() {
        let same: [Int64] = [100, 100, 100, 100]
        for v in same {
            XCTAssertEqual(HeatmapScale.level(tokens: v, allNonZero: same), 3)
        }
    }

    func test_quartileBoundariesAreDeterministic() {
        // Run twice on the same input; buckets must match.
        let values: [Int64] = (1...100).map { Int64($0 * 10) }
        let first = values.map { HeatmapScale.level(tokens: $0, allNonZero: values) }
        let second = values.map { HeatmapScale.level(tokens: $0, allNonZero: values) }
        XCTAssertEqual(first, second)
    }

    func test_quartileAssignment_levelsCover1Through4() {
        // Population of 100 distinct values should produce a mix of
        // all four non-zero levels (1, 2, 3, 4).
        let values: [Int64] = (1...100).map { Int64($0 * 10) }
        let levels = Set(values.map { HeatmapScale.level(tokens: $0, allNonZero: values) })
        XCTAssertTrue(levels.contains(1))
        XCTAssertTrue(levels.contains(2))
        XCTAssertTrue(levels.contains(3))
        XCTAssertTrue(levels.contains(4))
    }

    func test_zeroTokens_alwaysLevelZero() {
        let values: [Int64] = [50, 100, 150, 200]
        XCTAssertEqual(HeatmapScale.level(tokens: 0, allNonZero: values), 0)
    }

    func test_week_enumeratesSevenDays() {
        let days = Fmt.enumerateDays(from: Fmt.parseDateKey("2026-07-06")!,
                                     to:   Fmt.parseDateKey("2026-07-12")!)
        XCTAssertEqual(days.count, 7)
    }

    func test_leapYear_annualIncludesFeb29() {
        let cal = Calendar(identifier: .gregorian)
        let yearStart = cal.date(from: DateComponents(year: 2024, month: 1, day: 1))!
        let yearEnd = cal.date(from: DateComponents(year: 2025, month: 1, day: 1))!
        let days = Fmt.enumerateDays(from: yearStart, to: yearEnd.addingTimeInterval(-86_400), calendar: cal)
        // 2024 is a leap year → 366 days.
        XCTAssertEqual(days.count, 366)
        // Verify Feb 29 exists in the enumeration.
        let hasFeb29 = days.contains { d in
            let comps = cal.dateComponents([.year, .month, .day], from: d)
            return comps.month == 2 && comps.day == 29
        }
        XCTAssertTrue(hasFeb29, "leap-year enumeration must include Feb 29")
    }
}