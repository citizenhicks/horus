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

    func testBuildsWeeklyAndCumulativeProviderBars() {
        let usage = [
            usage(day: 100, provider: "openai", tokens: 4),
            usage(day: 106, provider: "openai", tokens: 6),
            usage(day: 107, provider: "openai", tokens: 3),
        ]

        let weekly = ProviderUsageSeries.points(
            from: usage,
            endingOn: 107,
            dayCount: 8,
            providerLabels: ["openai": "OpenAI"],
            aggregation: .weekly
        )
        XCTAssertEqual(weekly.map(\.unixDay), [100, 107])
        XCTAssertEqual(weekly.map(\.totalTokens), [10, 3])

        let cumulative = ProviderUsageSeries.points(
            from: usage,
            endingOn: 107,
            dayCount: 8,
            providerLabels: ["openai": "OpenAI"],
            aggregation: .cumulative
        )
        XCTAssertEqual(cumulative.map(\.unixDay), [107])
        XCTAssertEqual(cumulative.map(\.totalTokens), [13])
    }

    func testBuildsHeatmapValuesForEachAggregation() {
        let usage = [
            usage(day: 0, provider: "openai", tokens: 2),
            usage(day: 2, provider: "anthropic", tokens: 3),
            usage(day: 6, provider: "openai", tokens: 5),
        ]

        let weekly = UsageActivitySeries.snapshot(
            from: usage,
            endingOn: 6,
            weekCount: 1,
            aggregation: .weekly
        )
        XCTAssertEqual(weekly.values, Array(repeating: 10, count: 7))
        XCTAssertEqual(weekly.activeDays, 3)
        XCTAssertEqual(weekly.totalTokens, 10)

        let cumulative = UsageActivitySeries.snapshot(
            from: usage,
            endingOn: 6,
            weekCount: 1,
            aggregation: .cumulative
        )
        XCTAssertEqual(cumulative.values, [2, 2, 5, 5, 5, 5, 10])
        XCTAssertEqual(cumulative.maximum, 10)
    }

    private func usage(day: UInt64, provider: String, tokens: Int) -> DailyUsage {
        var usage = TokenUsage()
        usage.totalTokens = tokens
        return DailyUsage(unixDay: day, provider: provider, usage: usage)
    }
}
