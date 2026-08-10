import Charts
import Foundation
import SwiftUI

struct ProviderUsagePoint: Equatable, Identifiable {
    var id: String { "\(provider):\(unixDay)" }
    let unixDay: UInt64
    let provider: String
    let providerLabel: String
    let totalTokens: Int

    var date: Date {
        Date(timeIntervalSince1970: TimeInterval(unixDay) * 86_400)
    }
}

enum ProviderUsageSeries {
    static func points(
        from usage: [DailyUsage],
        endingOn endDay: UInt64,
        dayCount: Int,
        providerLabels: [String: String]
    ) -> [ProviderUsagePoint] {
        guard dayCount > 0 else { return [] }
        let requestedSpan = UInt64(dayCount - 1)
        let startDay = endDay - min(endDay, requestedSpan)
        let inRange = usage.filter {
            $0.unixDay >= startDay && $0.unixDay <= endDay && !$0.provider.isEmpty
        }
        let providers = Set(inRange.map(\.provider)).sorted {
            let left = providerLabels[$0] ?? $0
            let right = providerLabels[$1] ?? $1
            let comparison = left.localizedStandardCompare(right)
            return comparison == .orderedSame ? $0 < $1 : comparison == .orderedAscending
        }
        let totals = inRange.reduce(into: [ProviderDay: Int]()) { result, day in
            result[ProviderDay(provider: day.provider, unixDay: day.unixDay), default: 0]
                += day.usage.totalTokens
        }
        let actualDayCount = Int(endDay - startDay) + 1

        return providers.flatMap { provider in
            (0..<actualDayCount).map { offset in
                let unixDay = startDay + UInt64(offset)
                return ProviderUsagePoint(
                    unixDay: unixDay,
                    provider: provider,
                    providerLabel: providerLabels[provider] ?? provider,
                    totalTokens: totals[ProviderDay(provider: provider, unixDay: unixDay)] ?? 0
                )
            }
        }
    }

    private struct ProviderDay: Hashable {
        let provider: String
        let unixDay: UInt64
    }
}

struct ProviderUsageChart: View {
    @Environment(\.horusPalette) private var palette
    let usage: [DailyUsage]
    let providerLabels: [String: String]
    var weekCount = 25

    var body: some View {
        let points = ProviderUsageSeries.points(
            from: usage,
            endingOn: UInt64(Date.now.timeIntervalSince1970 / 86_400),
            dayCount: weekCount * 7,
            providerLabels: providerLabels
        )
        if points.contains(where: { $0.totalTokens > 0 }) {
            chart(points)
        } else {
            ContentUnavailableView(
                "No usage yet",
                systemImage: "chart.xyaxis.line",
                description: Text("Provider activity will appear after the first model call.")
            )
            .frame(maxWidth: .infinity, minHeight: 190)
        }
    }

    private func chart(_ points: [ProviderUsagePoint]) -> some View {
        Chart(points) { point in
            AreaMark(
                x: .value("Date", point.date, unit: .day),
                y: .value("Tokens", point.totalTokens),
                stacking: .unstacked
            )
            .foregroundStyle(by: .value("Provider", point.providerLabel))
            .interpolationMethod(.linear)
            .opacity(0.16)

            LineMark(
                x: .value("Date", point.date, unit: .day),
                y: .value("Tokens", point.totalTokens)
            )
            .foregroundStyle(by: .value("Provider", point.providerLabel))
            .interpolationMethod(.linear)
            .lineStyle(StrokeStyle(lineWidth: 2, lineCap: .round, lineJoin: .round))
        }
        .chartXAxis {
            AxisMarks(values: .stride(by: .month)) { _ in
                AxisGridLine().foregroundStyle(palette.line.opacity(0.3))
                AxisTick().foregroundStyle(palette.line)
                AxisValueLabel(format: .dateTime.month(.abbreviated))
                    .foregroundStyle(palette.muted)
            }
        }
        .chartYAxis {
            AxisMarks(position: .leading, values: .automatic(desiredCount: 4)) { value in
                AxisGridLine().foregroundStyle(palette.line.opacity(0.3))
                AxisValueLabel {
                    if let tokens = value.as(Int.self) {
                        Text(chartCompact(tokens))
                    }
                }
                .foregroundStyle(palette.muted)
            }
        }
        .chartLegend(position: .bottom, alignment: .leading, spacing: 10)
        .chartPlotStyle { plot in
            plot
                .background(palette.line.opacity(0.08))
                .clipShape(HorusStyle.controlShape)
        }
        .frame(height: 220)
        .accessibilityLabel("Daily token usage by provider")
    }
}

private func chartCompact(_ value: Int) -> String {
    value.formatted(.number.notation(.compactName).precision(.fractionLength(0 ... 1)))
}
