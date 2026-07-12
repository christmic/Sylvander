import XCTest
@testable import Token9

/// Pure aggregation pipeline tests. Cover the summary, daily, and
/// group-by behavior in `DashboardViewModel` without touching the
/// networking layer or SwiftUI.
final class DashboardAggregationTests: XCTestCase {

    // MARK: Summary

    func test_summary_totalsAcrossMultipleBuckets() {
        let buckets = [
            bucket(date: "2026-07-10", tool: "claude", model: "claude-3-5",
                   req: 3, inp: 100, out: 50, cr: 200, cw: 10),
            bucket(date: "2026-07-10", tool: "codex", model: "gpt-5",
                   req: 1, inp: 80, out: 20, cr: 0, cw: 0),
        ]
        let s = DashboardSummary.from(buckets)
        XCTAssertEqual(s.requests, 4)
        XCTAssertEqual(s.inputTokens, 180)
        XCTAssertEqual(s.cacheReadTokens, 200)
        // totalTokens = sum of input+output+cacheRead+cacheWrite per bucket.
        XCTAssertEqual(s.totalTokens, 100 + 50 + 200 + 10 + 80 + 20)
    }

    func test_summary_cacheHitPercent_zeroWhenDenominatorZero() {
        let buckets = [
            bucket(date: "2026-07-10", tool: "x", model: "y",
                   req: 1, inp: 0, out: 5, cr: 0, cw: 0),
        ]
        let s = DashboardSummary.from(buckets)
        XCTAssertEqual(s.cacheHitPercent, 0)
    }

    func test_summary_cacheHitPercent_computed() {
        let buckets = [
            bucket(date: "2026-07-10", tool: "x", model: "y",
                   req: 1, inp: 100, out: 0, cr: 300, cw: 0),
        ]
        let s = DashboardSummary.from(buckets)
        // 300 / (100 + 300) * 100 = 75
        XCTAssertEqual(s.cacheHitPercent, 75)
    }

    // MARK: Daily aggregation

    func test_daily_combinesProviderToolModelBucketsOnSameDay() {
        let buckets = [
            bucket(date: "2026-07-10", tool: "claude", model: "claude-3-5",
                   req: 2, inp: 10, out: 5, cr: 100, cw: 0),
            bucket(date: "2026-07-10", tool: "codex", model: "gpt-5",
                   req: 3, inp: 20, out: 10, cr: 0, cw: 0),
        ]
        let daily = DashboardAggregator.aggregateDaily(
            buckets: buckets, from: "2026-07-10", to: "2026-07-10"
        )
        XCTAssertEqual(daily.count, 1)
        XCTAssertEqual(daily[0].tokens, 10 + 5 + 100 + 20 + 10)
        XCTAssertEqual(daily[0].requests, 5)
        XCTAssertTrue(daily[0].hasData)
    }

    func test_daily_fillsMissingCalendarDaysWithZero() {
        // Range covers three days; only the middle day has buckets.
        let buckets = [
            bucket(date: "2026-07-11", tool: "x", model: "y",
                   req: 1, inp: 10, out: 0, cr: 0, cw: 0),
        ]
        let daily = DashboardAggregator.aggregateDaily(
            buckets: buckets, from: "2026-07-10", to: "2026-07-12"
        )
        XCTAssertEqual(daily.count, 3)
        XCTAssertEqual(daily[0].tokens, 0)
        XCTAssertEqual(daily[0].requests, 0)
        XCTAssertTrue(daily[0].hasData, "filled days should still report hasData=true")
        XCTAssertEqual(daily[1].tokens, 10)
        XCTAssertEqual(daily[2].tokens, 0)
    }

    func test_daily_dimensionToggle_doesNotChangeTotals() {
        // The daily array is independent of groupBy — same buckets in
        // produce same daily out regardless of tool/model toggle.
        let buckets = [
            bucket(date: "2026-07-10", tool: "claude", model: "claude-3-5",
                   req: 1, inp: 10, out: 5, cr: 0, cw: 0),
        ]
        let a = DashboardAggregator.aggregateDaily(buckets: buckets, from: "2026-07-10", to: "2026-07-10")
        let b = DashboardAggregator.aggregateDaily(buckets: buckets, from: "2026-07-10", to: "2026-07-10")
        XCTAssertEqual(a.map(\.tokens), b.map(\.tokens))
    }

    // MARK: Group aggregation

    func test_group_sharesSumToApprox100WhenNonEmpty() {
        let buckets = [
            bucket(date: "2026-07-10", tool: "claude", model: "m", req: 1, inp: 100, out: 0, cr: 0, cw: 0),
            bucket(date: "2026-07-10", tool: "codex", model: "m", req: 1, inp: 300, out: 0, cr: 0, cw: 0),
        ]
        let groups = DashboardAggregator.aggregate(buckets: buckets, groupBy: .tool, rateLimits: [])
        let total = groups.reduce(Int64(0)) { $0 + $1.totalTokens }
        let sumShares = groups.reduce(0.0) { $0 + $1.sharePercent(allTotal: total) }
        XCTAssertEqual(sumShares, 100.0, accuracy: 0.01)
    }

    func test_group_orderDescending_stableTiebreak() {
        let buckets = [
            bucket(date: "2026-07-10", tool: "beta",  model: "m", req: 1, inp: 100, out: 0, cr: 0, cw: 0),
            bucket(date: "2026-07-10", tool: "alpha", model: "m", req: 1, inp: 200, out: 0, cr: 0, cw: 0),
            bucket(date: "2026-07-10", tool: "alpha", model: "m", req: 1, inp: 0,   out: 0, cr: 0, cw: 0),
            bucket(date: "2026-07-10", tool: "gamma", model: "m", req: 1, inp: 100, out: 0, cr: 0, cw: 0),
        ]
        let groups = DashboardAggregator.aggregate(buckets: buckets, groupBy: .tool, rateLimits: [])
        // Stable sort: desc by totalTokens, asc by name for ties.
        let sorted = groups.sorted {
            if $0.totalTokens != $1.totalTokens { return $0.totalTokens > $1.totalTokens }
            return $0.name.localizedCompare($1.name) == .orderedAscending
        }
        XCTAssertEqual(sorted.map(\.name), ["alpha", "beta", "gamma"])
        // alpha has total 200, beta+gamma both 100; tiebreak = name asc
        // → beta (100) before gamma (100).
    }

    // MARK: Rate limit

    func test_rateLimit_floorAcrossPairs() {
        let rates = [
            RateLimitDto(
                provider: "anthropic", updated_at: 0,
                requests_limit: 100, requests_remaining: 90, requests_reset: nil,
                tokens_limit: 1000, tokens_remaining: 50, tokens_reset: nil
            ),
            RateLimitDto(
                provider: "openai", updated_at: 0,
                requests_limit: 100, requests_remaining: 10, requests_reset: nil,
                tokens_limit: 1000, tokens_remaining: nil, tokens_reset: nil
            ),
        ]
        let min = DashboardAggregator.minRateLimit(rates)
        // anthropic requests: 90/100 = 90%; tokens: 50/1000 = 5%  → 5%
        // openai requests:    10/100 = 10%; tokens: nil            → 10%
        // overall floor:      5%
        XCTAssertEqual(min, 5.0)
    }

    func test_rateLimit_nilWhenNoData() {
        XCTAssertNil(DashboardAggregator.minRateLimit([]))
    }

    // MARK: Helpers

    private func bucket(date: String, tool: String, model: String,
                        req: Int64, inp: Int64, out: Int64, cr: Int64, cw: Int64) -> StatBucketDto {
        StatBucketDto(
            provider: "test", model: model, tool: tool, date: date,
            requests: req, input_tokens: inp, output_tokens: out,
            cache_read_tokens: cr, cache_write_tokens: cw, cache_ratio: 0
        )
    }
}