import XCTest

final class ProviderUsageChartTests: XCTestCase {
    func testBuildsZeroFilledProviderSeriesAndCombinesDuplicateRows() {
        let points = ProviderUsageSeries.points(
            from: [
                usage(day: 100, provider: "openai", tokens: 20),
                usage(day: 100, provider: "openai", tokens: 5),
                usage(day: 101, provider: "anthropic", tokens: 9),
                usage(day: 90, provider: "openai", tokens: 999),
            ],
            endingOn: 102,
            dayCount: 3,
            providerLabels: ["anthropic": "Anthropic", "openai": "OpenAI"]
        )

        XCTAssertEqual(points.count, 6)
        XCTAssertEqual(points.map(\.provider), [
            "anthropic", "anthropic", "anthropic",
            "openai", "openai", "openai",
        ])
        XCTAssertEqual(points.map(\.unixDay), [100, 101, 102, 100, 101, 102])
        XCTAssertEqual(points.map(\.totalTokens), [0, 9, 0, 25, 0, 0])
        XCTAssertEqual(points.first?.providerLabel, "Anthropic")
        XCTAssertEqual(points.last?.providerLabel, "OpenAI")
    }

    private func usage(day: UInt64, provider: String, tokens: Int) -> DailyUsage {
        var usage = TokenUsage()
        usage.totalTokens = tokens
        return DailyUsage(unixDay: day, provider: provider, usage: usage)
    }
}
