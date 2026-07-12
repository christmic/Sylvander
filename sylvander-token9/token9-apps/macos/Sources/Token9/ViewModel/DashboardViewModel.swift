import Foundation
import SwiftUI

// MARK: - Value types

/// Top-of-dashboard summary strip. Aggregated across every bucket in
/// the selected range, regardless of the tool/model grouping.
struct DashboardSummary: Equatable {
    let totalTokens: Int64
    let requests: Int64
    let cacheReadTokens: Int64
    let inputTokens: Int64
    let cacheHitPercent: Double

    static let zero = DashboardSummary(
        totalTokens: 0, requests: 0, cacheReadTokens: 0,
        inputTokens: 0, cacheHitPercent: 0
    )

    /// Build a summary from a flat list of buckets.
    static func from(_ buckets: [StatBucketDto]) -> DashboardSummary {
        var total: Int64 = 0, req: Int64 = 0, cr: Int64 = 0, inp: Int64 = 0
        for b in buckets {
            total += b.input_tokens + b.output_tokens + b.cache_read_tokens + b.cache_write_tokens
            req += b.requests
            cr += b.cache_read_tokens
            inp += b.input_tokens
        }
        let denom = inp + cr
        let hit = denom > 0 ? Double(cr) / Double(denom) * 100.0 : 0.0
        return DashboardSummary(
            totalTokens: total, requests: req,
            cacheReadTokens: cr, inputTokens: inp,
            cacheHitPercent: hit
        )
    }
}

/// One calendar day's totals. Independent of the tool/model grouping;
/// the heatmap is always built from this array.
struct DailyUsage: Identifiable, Equatable {
    let date: Date
    let dateKey: String
    let tokens: Int64
    let requests: Int64
    /// False only when the whole request failed and we have no API
    /// data for this day at all. Filled days with no buckets are true
    /// with tokens=0 (the API succeeded, the day had no traffic).
    let hasData: Bool
    var id: String { dateKey }
}

/// A tool or model row. Same shape as the v1 GroupCard, kept for diff
/// minimality. Adds sharePercent for the compact row UI in commit 5.
struct GroupCard: Identifiable {
    let id: String
    let name: String
    let requests: Int64
    let input: Int64
    let output: Int64
    let cacheRead: Int64
    let cacheWrite: Int64
    let subs: [SubLine]
    let rateLimits: [RateLimitDto]

    var totalTokens: Int64 { input + output + cacheRead + cacheWrite }
    var cacheRatio: Double {
        let denom = input + cacheRead
        return denom > 0 ? Double(cacheRead) / Double(denom) : 0
    }
    /// sharePercent in [0, 100] computed against the sum of all group
    /// totals. Returns 0 when `allTotal == 0` (avoids NaN).
    func sharePercent(allTotal: Int64) -> Double {
        guard allTotal > 0 else { return 0 }
        return Double(totalTokens) / Double(allTotal) * 100.0
    }
}

struct SubLine: Identifiable {
    let id: String
    let name: String
    let tokens: Int64
    let requests: Int64
    let cacheRatio: Double
}

/// Group-by selector for the row aggregation. The secondary dimension
/// used inside expanded rows is the inverse of the primary dimension
/// (tool -> secondary model, model -> secondary tool).
enum GroupBy: String, CaseIterable, Identifiable {
    case tool, model
    var id: String { rawValue }
    var label: String { self == .tool ? "工具" : "模型" }
    var subTitle: String { self == .tool ? "按模型" : "按工具" }
    var subKey: String { self == .tool ? "model" : "tool" }
}

// MARK: - Pure aggregation namespace
//
// Pulled out of the @MainActor view model so tests can call them
// synchronously without an actor hop. All three functions are
// deterministic given their inputs and safe to invoke from any
// thread.
enum DashboardAggregator {
    /// Aggregate buckets by dateKey, then fill every calendar day in
    /// the [from, to] range with hasData=true and zeros.
    static func aggregateDaily(buckets: [StatBucketDto], from: String, to: String) -> [DailyUsage] {
        let cal = Calendar.current
        let fromDate = Fmt.parseDateKey(from) ?? Date()
        let toDate = Fmt.parseDateKey(to) ?? Date()
        var byKey: [String: (tokens: Int64, requests: Int64)] = [:]
        for b in buckets {
            let entry = byKey[b.date] ?? (0, 0)
            byKey[b.date] = (
                entry.tokens + b.input_tokens + b.output_tokens + b.cache_read_tokens + b.cache_write_tokens,
                entry.requests + b.requests
            )
        }
        let days = Fmt.enumerateDays(from: fromDate, to: toDate, calendar: cal)
        return days.map { d in
            let key = Fmt.dateKey(d, calendar: cal)
            let entry = byKey[key] ?? (0, 0)
            return DailyUsage(
                date: d, dateKey: key,
                tokens: entry.tokens, requests: entry.requests,
                hasData: true
            )
        }
    }

    /// Group-by aggregation. The primary key is `groupBy == .tool ? tool : model`.
    /// The secondary key inside SubLine is the inverse. Rate limits are
    /// matched by provider name (the closest stable identity in the
    /// current data model).
    static func aggregate(buckets: [StatBucketDto], groupBy: GroupBy, rateLimits: [RateLimitDto]) -> [GroupCard] {
        var groups: [String: (input: Int64, output: Int64, cr: Int64, cw: Int64, req: Int64, providers: Set<String>, subs: [String: (name: String, tokens: Int64, req: Int64, input: Int64, cr: Int64)])] = [:]
        let primaryKey: KeyPath<StatBucketDto, String> = (groupBy == .tool) ? \.tool : \.model
        let subKey: KeyPath<StatBucketDto, String> = (groupBy == .tool) ? \.model : \.tool

        for b in buckets {
            let name = b[keyPath: primaryKey]
            guard !name.isEmpty else { continue }
            var g = groups[name] ?? (0, 0, 0, 0, 0, [], [:])
            g.input += b.input_tokens
            g.output += b.output_tokens
            g.cr += b.cache_read_tokens
            g.cw += b.cache_write_tokens
            g.req += b.requests
            g.providers.insert(b.provider)
            let sName = b[keyPath: subKey]
            if !sName.isEmpty && sName != name {
                var s = g.subs[sName] ?? (sName, 0, 0, 0, 0)
                s.tokens += b.input_tokens + b.output_tokens + b.cache_read_tokens + b.cache_write_tokens
                s.req += b.requests
                s.input += b.input_tokens
                s.cr += b.cache_read_tokens
                g.subs[sName] = s
            }
            groups[name] = g
        }

        return groups.map { name, g in
            let subs: [SubLine] = g.subs.values
                .sorted {
                    if $0.tokens != $1.tokens { return $0.tokens > $1.tokens }
                    return $0.name.localizedCompare($1.name) == .orderedAscending
                }
                .map {
                    let denominator = $0.input + $0.cr
                    let ratio = denominator > 0 ? Double($0.cr) / Double(denominator) : 0
                    return SubLine(id: $0.name, name: $0.name, tokens: $0.tokens, requests: $0.req, cacheRatio: ratio)
                }
            let rates = rateLimits.filter { g.providers.contains($0.provider) }
            return GroupCard(
                id: name, name: name,
                requests: g.req,
                input: g.input, output: g.output,
                cacheRead: g.cr, cacheWrite: g.cw,
                subs: subs, rateLimits: rates
            )
        }
    }

    /// Lowest remaining percent across any rate-limit pair. A pair with
    /// either limit missing is ignored. Returns nil when no usable data.
    static func minRateLimit(_ rates: [RateLimitDto]) -> Double? {
        var lo: Double?
        for r in rates {
            let pairs: [(Int32?, Int32?)] = [
                (r.requests_remaining, r.requests_limit),
                (r.tokens_remaining,   r.tokens_limit),
            ]
            for (rem, lim) in pairs {
                guard let rem, let lim, lim > 0 else { continue }
                let pct = Double(rem) / Double(lim) * 100.0
                if lo == nil || pct < lo! { lo = pct }
            }
        }
        return lo
    }
}

// MARK: - View model

/// Owns the dashboard's data + UI state. Refreshes every 30 seconds.
/// Holds onto the last successful snapshot so refresh failures don't
/// blank the UI (per checklist §4 B4).
@MainActor
final class DashboardViewModel: ObservableObject {
    @Published var range: RangeKey = .today { didSet { if oldValue != range { Task { await load() } } } }
    @Published var groupBy: GroupBy = .tool {
        didSet { if oldValue != groupBy { rebuildGroups() } }
    }
    @Published var cards: [GroupCard] = []
    @Published var daily: [DailyUsage] = []
    @Published var summary: DashboardSummary = .zero
    @Published var updatedAt: Date?
    @Published var loading = false
    @Published var error: String?
    /// Minimum remaining percent across all rate-limit pairs (nil when
    /// rate-limit headers are absent). Drives the warning footer
    /// implemented in commit 6.
    @Published var minimumRateLimitPercent: Double?
    @Published private(set) var hasSuccessfulLoad = false

    private let client: Token9Client
    private var timer: Timer?
    private var lastSuccessfulBuckets: [StatBucketDto] = []
    private var lastHeatmapBuckets: [StatBucketDto] = []
    private var lastSuccessfulRateLimits: [RateLimitDto] = []
    private var lastQueryFrom: String = ""
    private var lastQueryTo: String = ""
    private var heatmapQueryFrom: String = ""
    private var heatmapQueryTo: String = ""
    private var loadedRange: RangeKey?

    init(client: Token9Client = Token9Client()) {
        self.client = client
    }

    func start() {
        Task { await load() }
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { [weak self] _ in
            Task { await self?.load() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    func reload() {
        Task { await load() }
    }

    // MARK: Load + aggregate

    private func load() async {
        loading = true
        defer { loading = false }
        let (from, to) = range.range()
        let heatmapRange = range.heatmapRange()
        do {
            async let selectedCall = client.stats(from: from, to: to)
            async let heatmapCall = client.stats(from: heatmapRange.from, to: heatmapRange.to)
            let buckets = try await selectedCall.buckets
            let heatmapBuckets = try await heatmapCall.buckets
            let rates = (try? await client.rateLimits().rate_limits) ?? []
            lastSuccessfulBuckets = buckets
            lastHeatmapBuckets = heatmapBuckets
            lastSuccessfulRateLimits = rates
            lastQueryFrom = from
            lastQueryTo = to
            heatmapQueryFrom = heatmapRange.from
            heatmapQueryTo = heatmapRange.to
            loadedRange = range
            hasSuccessfulLoad = true
            error = nil
            rebuild()
        } catch {
            // Per checklist §4 B4: keep last data visible on refresh
            // failure; only show error when there is no cached state.
            if lastSuccessfulBuckets.isEmpty || loadedRange != range {
                self.error = error.localizedDescription
                self.cards = []
                self.daily = []
                self.summary = .zero
            } else {
                self.error = error.localizedDescription
                // Keep last successful data; updatedAt unchanged so the
                // header can show "last successful: …" in commit 6.
            }
        }
    }

    private func rebuild() {
        let buckets = lastSuccessfulBuckets
        let rates = lastSuccessfulRateLimits

        // 1. Summary (always computed over every bucket).
        summary = DashboardSummary.from(buckets)

        // 2. Daily aggregation — independent of groupBy so the heatmap
        // doesn't recolor on tool/model toggle (checklist §6 D).
        daily = DashboardAggregator.aggregateDaily(
            buckets: lastHeatmapBuckets, from: heatmapQueryFrom, to: heatmapQueryTo
        )

        // 3. Group aggregation with stable tiebreak.
        let grouped = DashboardAggregator.aggregate(
            buckets: buckets, groupBy: groupBy, rateLimits: rates
        )
        // Sort desc by totalTokens, then asc by name for stability.
        cards = grouped.sorted {
            if $0.totalTokens != $1.totalTokens { return $0.totalTokens > $1.totalTokens }
            return $0.name.localizedCompare($1.name) == .orderedAscending
        }

        // 4. Rate-limit floor.
        minimumRateLimitPercent = DashboardAggregator.minRateLimit(rates)

        updatedAt = Date()
    }

    private func rebuildGroups() {
        let grouped = DashboardAggregator.aggregate(
            buckets: lastSuccessfulBuckets,
            groupBy: groupBy,
            rateLimits: lastSuccessfulRateLimits
        )
        cards = grouped.sorted {
            if $0.totalTokens != $1.totalTokens { return $0.totalTokens > $1.totalTokens }
            return $0.name.localizedCompare($1.name) == .orderedAscending
        }
    }
}
