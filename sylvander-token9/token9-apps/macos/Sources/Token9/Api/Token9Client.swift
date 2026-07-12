import Foundation

/// Thin async client over the token9 HTTP API. Types come from the generated
/// contracts (Rust is the single source of truth).
struct Token9Client {
    var baseURL: URL

    init(baseURL: URL = URL(string: "http://127.0.0.1:9527")!) {
        self.baseURL = baseURL
    }

    func stats(from: String? = nil, to: String? = nil) async throws -> StatsResponse {
        var comps = URLComponents(
            url: baseURL.appendingPathComponent("stats/summary"),
            resolvingAgainstBaseURL: false)!
        var items: [URLQueryItem] = []
        if let from { items.append(URLQueryItem(name: "from", value: from)) }
        if let to { items.append(URLQueryItem(name: "to", value: to)) }
        if !items.isEmpty { comps.queryItems = items }
        return try await get(comps.url!)
    }

    func rateLimits() async throws -> RateLimitsResponse {
        try await get(baseURL.appendingPathComponent("ratelimits"))
    }

    private func get<T: Decodable>(_ url: URL) async throws -> T {
        let (data, resp) = try await URLSession.shared.data(from: url)
        guard let http = resp as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
        return try JSONDecoder().decode(T.self, from: data)
    }
}
