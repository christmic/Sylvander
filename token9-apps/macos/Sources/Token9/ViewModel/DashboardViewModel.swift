import Foundation
import SwiftUI

/// Aggregation dimension for the cards.
enum GroupBy: String, CaseIterable, Identifiable {
    case tool, model
    var id: String { rawValue }
    var label: String { self == .tool ? "工具" : "模型" }
    var subTitle: String { self == .tool ? "按模型" : "按工具" }
}

/// A breakdown line inside a card (the other dimension).
struct SubLine: Identifiable {
    let id = UUID()
    let name: String
    let tokens: Int64
    let requests: Int64
    let cacheRatio: Double
}

/// A card aggregated by the selected dimension (tool or model).
struct GroupCard: Identifiable {
    let id: String
    let name: String
    let tint: Color
    var requests: Int64 = 0
    var input: Int64 = 0
    var output: Int64 = 0
    var cacheRead: Int64 = 0
    var cacheWrite: Int64 = 0
    var subs: [SubLine] = []
    var rateLimits: [RateLimitDto] = []

    var totalTokens: Int64 { input + output + cacheRead + cacheWrite }
    var cacheRatioPct: Double {
        let denom = input + cacheRead
        return denom > 0 ? Double(cacheRead) / Double(denom) * 100 : 0
    }
}

@MainActor
final class DashboardViewModel: ObservableObject {
    @Published var range: RangeKey = .today { didSet { reload() } }
    @Published var groupBy: GroupBy = .tool { didSet { rebuild() } }
    @Published var cards: [GroupCard] = []
    @Published var updatedAt: Date?
    @Published var loading = false
    @Published var error: String?

    private let client = Token9Client()
    private var timer: Timer?
    private var buckets: [StatBucketDto] = []
    private var rateLimits: [RateLimitDto] = []

    func start() {
        reload()
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { [weak self] _ in
            guard let self else { return }
            Task { @MainActor in self.reload() }
        }
    }

    func stop() { timer?.invalidate(); timer = nil }

    func reload() { Task { await load() } }

    func load() async {
        loading = true
        error = nil
        let (from, to) = range.range()
        do {
            async let statsCall = client.stats(from: from, to: to)
            async let rlCall = client.rateLimits()
            buckets = try await statsCall.buckets
            rateLimits = (try? await rlCall)?.rate_limits ?? []
            rebuild()
            updatedAt = Date()
        } catch {
            self.error = error.localizedDescription
        }
        loading = false
    }

    private func rebuild() {
        cards = Self.aggregate(buckets, groupBy: groupBy, rateLimits: rateLimits)
    }

    static func aggregate(_ buckets: [StatBucketDto], groupBy: GroupBy, rateLimits: [RateLimitDto]) -> [GroupCard] {
        struct Acc {
            var requests: Int64 = 0
            var input: Int64 = 0
            var output: Int64 = 0
            var cacheRead: Int64 = 0
            var cacheWrite: Int64 = 0
            var providers: Set<String> = []
            var subs: [String: SubAcc] = [:]
        }
        struct SubAcc { var tokens: Int64 = 0; var requests: Int64 = 0; var input: Int64 = 0; var cacheRead: Int64 = 0 }

        var groups: [String: Acc] = [:]
        for b in buckets {
            let primary = groupBy == .tool ? b.tool : b.model
            let secondary = groupBy == .tool ? b.model : b.tool
            var g = groups[primary] ?? Acc()
            g.requests += b.requests
            g.input += b.input_tokens
            g.output += b.output_tokens
            g.cacheRead += b.cache_read_tokens
            g.cacheWrite += b.cache_write_tokens
            g.providers.insert(b.provider)
            var s = g.subs[secondary] ?? SubAcc()
            s.tokens += b.input_tokens + b.output_tokens + b.cache_read_tokens + b.cache_write_tokens
            s.requests += b.requests
            s.input += b.input_tokens
            s.cacheRead += b.cache_read_tokens
            g.subs[secondary] = s
            groups[primary] = g
        }

        var rlByProvider: [String: RateLimitDto] = [:]
        for rl in rateLimits { rlByProvider[rl.provider] = rl }

        return groups.map { name, g in
            let tint = groupBy == .tool ? Theme.toolTint(name) : Theme.paletteColor(name)
            let subs = g.subs.map { sn, s -> SubLine in
                let denom = s.input + s.cacheRead
                return SubLine(name: sn, tokens: s.tokens, requests: s.requests,
                               cacheRatio: denom > 0 ? Double(s.cacheRead) / Double(denom) * 100 : 0)
            }.sorted { $0.tokens > $1.tokens }
            return GroupCard(
                id: name, name: name, tint: tint,
                requests: g.requests, input: g.input, output: g.output,
                cacheRead: g.cacheRead, cacheWrite: g.cacheWrite,
                subs: subs,
                rateLimits: g.providers.compactMap { rlByProvider[$0] })
        }
        .sorted { $0.totalTokens > $1.totalTokens }
    }
}
